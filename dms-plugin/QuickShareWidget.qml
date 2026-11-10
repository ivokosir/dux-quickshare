import QtQuick
import QtQuick.Shapes
import Quickshell
import Quickshell.Io
import Quickshell.Widgets
import qs.Common
import qs.DCommon.Widgets
import qs.Modules.Plugins

PluginComponent {
    id: root

    layerNamespacePlugin: "quickshare"

    property string bin: Quickshell.env("HOME") + "/.local/bin/qsbar"
    property var st: ({ daemon: false })
    property real nowMs: Date.now()

    readonly property bool up: st.daemon === true
    readonly property bool visibleNow: up && st.visible === true
    readonly property bool temporary: visibleNow && !!st.visible_until
    readonly property bool nearby: up && st.nearby === true && !visibleNow
    readonly property var incoming: up && st.incoming ? st.incoming : []
    readonly property string deviceName: st.device_name || "this PC"

    // The lists the popout draws. A new array rebuilds every row and button, and
    // the state arrives several times a second during a transfer, so a list is
    // only replaced when it really changed. Running transfers are listed by id
    // and read their numbers from `byId`.
    property var waiting: []
    property var activeIds: []
    property var history: []
    property var trusted: []
    property var byId: ({})
    property var _lastById: ({})
    property var _sigs: ({})

    function setList(name, value) {
        const sig = JSON.stringify(value);
        if (root._sigs[name] === sig)
            return;
        root._sigs[name] = sig;
        root[name] = value;
    }

    function applyState(next) {
        const isUp = next.daemon === true;
        const inc = isUp && next.incoming ? next.incoming : [];
        const map = {};
        for (const t of inc)
            map[t.id] = t;
        // Transfers that just ended stay readable for one more round, while their
        // card is being taken down.
        root.byId = Object.assign({}, root._lastById, map);
        root._lastById = map;
        root.st = next;
        root.setList("waiting", inc.filter(i => i.state === "waiting"));
        root.setList("activeIds", inc.filter(i => i.state !== "waiting").map(i => i.id));
        root.setList("history", isUp && next.history ? next.history : []);
        root.setList("trusted", isUp && next.trusted ? next.trusted : []);
    }

    readonly property real activeProgress: {
        let total = 0, done = 0;
        for (const t of incoming) {
            if (t.state === "waiting")
                continue;
            total += Number(t.total_bytes) || 0;
            done += Number(t.ack_bytes) || 0;
        }
        return total > 0 ? Math.min(1, done / total) : 0;
    }

    // Recent: 5 rows, or all of them after "Show N more".
    property bool showAll: false
    // "Delete all?" is being asked in the list header.
    property bool confirmClear: false

    onInteractionActiveChanged: {
        if (!interactionActive) {
            root.confirmClear = false;
            root.showAll = false;
        }
    }

    // A command that failed says so (in the same spot as "Copied").
    property string failNote: ""
    // Kept while the message fades out: [text, is it bad].
    property var lastNote: ["", false]
    onNoteChanged: if (note !== "") lastNote = [note, noteBad]

    function run(args) {
        Proc.runCommand(null, [root.bin].concat(args), (out, code) => {
            if (code !== 0) {
                root.failNote = "That didn't work";
                failTimer.restart();
            }
        }, 0, 30000, root);
    }

    Timer {
        id: failTimer
        interval: 4000
        onTriggered: root.failNote = ""
    }

    // What the status chip says: the daemon's message, or a failed click.
    readonly property string note: up && st.status ? st.status : failNote
    readonly property bool noteBad: up && st.status ? st.status_bad === true : failNote !== ""

    function fileUrl(path) {
        return "file://" + String(path).split("/").map(encodeURIComponent).join("/");
    }

    function fmtSize(bytes) {
        const b = Number(bytes) || 0;
        if (b < 1024)
            return b + " B";
        if (b < 1024 * 1024)
            return Math.round(b / 1024) + " KB";
        if (b < 1024 * 1024 * 1024)
            return (b / 1024 / 1024).toFixed(1) + " MB";
        return (b / 1024 / 1024 / 1024).toFixed(2) + " GB";
    }

    function ago(ms) {
        const s = Math.max(0, Math.floor((root.nowMs - Number(ms)) / 1000));
        if (s < 45)
            return "just now";
        const m = Math.round(s / 60);
        if (m < 60)
            return m + " min ago";
        const h = Math.round(m / 60);
        if (h < 24)
            return h + " h ago";
        return Math.round(h / 24) + " d ago";
    }

    function countdown(until) {
        const s = Math.max(0, Math.ceil((Number(until) - root.nowMs) / 1000));
        const m = Math.floor(s / 60);
        return m + ":" + String(s % 60).padStart(2, "0");
    }

    function kindIcon(kind) {
        switch (kind) {
        case "image":
            return "image";
        case "url":
            return "link";
        case "wifi":
            return "wifi";
        case "text":
            return "notes";
        default:
            return "description";
        }
    }

    function filesCount(t) {
        return t && t.files ? t.files.length : 0;
    }

    function requestWhat(t) {
        const n = root.filesCount(t);
        if (n === 0)
            return t.text ? "“" + t.text + "”" : "text or a link";
        if (n === 1)
            return t.files[0];
        return n + (t.kind === "images" ? " photos" : " files");
    }

    function subtitleOf(item) {
        const parts = [item.device, root.ago(item.time)];
        if (item.kind === "image" || item.kind === "file")
            parts.push(root.fmtSize(item.size));
        if (item.saved)
            parts.push("in Downloads");
        else if (item.kind === "image" || item.kind === "file")
            parts.push("not saved");
        return parts.join(" · ");
    }

    readonly property string statusLine: {
        if (!up)
            return "Receiver is not running";
        if (temporary)
            return "Visible for " + countdown(st.visible_until) + " as " + deviceName;
        if (visibleNow)
            return "Visible as " + deviceName;
        return "Hidden · phones can't see this PC";
    }

    // ---- daemon connection -------------------------------------------------

    Process {
        id: watcher

        command: [root.bin, "watch"]
        running: true
        stdout: SplitParser {
            onRead: data => {
                try {
                    root.applyState(JSON.parse(data));
                } catch (e) {}
            }
        }
        onExited: {
            root.applyState({ daemon: false });
            restartWatcher.start();
        }
    }

    Timer {
        id: restartWatcher
        interval: 3000
        onTriggered: watcher.running = true
    }

    // The clock behind "Visible for 4:12" and "5 min ago": only while one of them shows.
    Timer {
        interval: 1000
        repeat: true
        running: root.temporary || (root.interactionActive && root.history.length > 0)
        triggeredOnStart: true
        onTriggered: root.nowMs = Date.now()
    }

    pillRightClickAction: () => root.run([root.visibleNow ? "hidden" : "visible"])

    // dms ipc call quickshare open | close | toggle | visibility  (bind these in niri)
    IpcHandler {
        target: "quickshare"

        function open(): string {
            root.triggerPopout();
            return "ok";
        }

        function close(): string {
            root.closePopout();
            return "ok";
        }

        function visibility(): string {
            root.run([root.visibleNow ? "hidden" : "visible"]);
            return root.visibleNow ? "hidden" : "visible";
        }
    }

    // ---- bar pill -----------------------------------------------------------

    readonly property color pillColor: {
        if (!up)
            return Theme.error;
        if (waiting.length > 0 || activeIds.length > 0)
            return Theme.primary;
        if (nearby)
            return Theme.warning;
        return visibleNow ? Theme.surfaceText : Theme.surfaceVariantText;
    }

    readonly property string pillLabel: {
        if (activeIds.length > 0)
            return Math.round(activeProgress * 100) + "%";
        if (temporary)
            return Math.max(1, Math.ceil((Number(st.visible_until) - nowMs) / 60000)) + "m";
        return "";
    }

    readonly property bool pulsing: waiting.length > 0 || nearby

    // Quick Share mark: two arrows inside a broken circle (24x24 grid).
    // A slash is added when hidden.
    component QsGlyph: Item {
        id: g

        property real size: 24
        property color color: Theme.surfaceText
        property bool slashed: false
        property real stroke: 2.1

        width: size
        height: size

        Shape {
            width: 24
            height: 24
            scale: g.size / 24
            transformOrigin: Item.TopLeft
            preferredRendererType: Shape.CurveRenderer

            ShapePath {
                strokeColor: g.color
                strokeWidth: g.stroke
                fillColor: "transparent"
                capStyle: ShapePath.RoundCap
                joinStyle: ShapePath.RoundJoin
                PathSvg {
                    path: "M 3.35 9.5 A 9 9 0 0 1 20.86 10.44 M 20.65 14.5 A 9 9 0 0 1 3.14 13.56 "
                        + "M 3.35 9.5 L 15.2 9.5 M 12.4 6.7 L 15.2 9.5 L 12.4 12.3 "
                        + "M 20.65 14.5 L 8.8 14.5 M 11.6 11.7 L 8.8 14.5 L 11.6 17.3"
                }
            }

            ShapePath {
                strokeColor: g.slashed ? g.color : "transparent"
                strokeWidth: g.stroke
                fillColor: "transparent"
                capStyle: ShapePath.RoundCap
                PathSvg {
                    path: "M 3.5 3.5 L 20.5 20.5"
                }
            }
        }
    }

    component PillIcon: QsGlyph {
        id: pillGlyph

        size: root.iconSize
        color: root.pillColor
        slashed: root.up && !root.visibleNow
        opacity: root.up && !root.visibleNow && !root.pulsing ? 0.6 : 1

        SequentialAnimation on scale {
            running: root.pulsing
            loops: Animation.Infinite
            onRunningChanged: if (!running) pillGlyph.scale = 1
            NumberAnimation { to: 1.18; duration: 420; easing.type: Easing.OutQuad }
            NumberAnimation { to: 1.0; duration: 420; easing.type: Easing.InQuad }
            PauseAnimation { duration: 500 }
        }
    }

    horizontalBarPill: Component {
        Row {
            spacing: Theme.spacingXS

            PillIcon {
                anchors.verticalCenter: parent.verticalCenter
            }

            StyledText {
                visible: root.pillLabel !== ""
                text: root.pillLabel
                font.pixelSize: root.textSize
                font.features: { "tnum": 1 }
                color: root.pillColor
                anchors.verticalCenter: parent.verticalCenter
            }
        }
    }

    verticalBarPill: Component {
        Column {
            spacing: 2

            PillIcon {
                anchors.horizontalCenter: parent.horizontalCenter
            }

            StyledText {
                visible: root.pillLabel !== ""
                text: root.pillLabel
                font.pixelSize: Theme.fontSizeSmall
                color: root.pillColor
                anchors.horizontalCenter: parent.horizontalCenter
            }
        }
    }

    // ---- small building blocks ----------------------------------------------

    component Btn: Rectangle {
        id: btn

        property string label
        property string icon
        property string variant: "tonal" // filled | tonal | text
        readonly property color fg: variant === "filled" ? Theme.onPrimary : Theme.primary
        signal clicked

        implicitHeight: 34
        implicitWidth: btnRow.implicitWidth + Theme.spacingM * 2
        radius: height / 2
        color: variant === "filled" ? Theme.primary : variant === "tonal" ? Theme.withAlpha(Theme.primary, 0.14) : "transparent"

        Rectangle {
            anchors.fill: parent
            radius: parent.radius
            color: btn.fg
            opacity: btnMouse.pressed ? 0.16 : btnMouse.containsMouse ? 0.08 : 0
        }

        Row {
            id: btnRow
            anchors.centerIn: parent
            spacing: 6

            DIcon {
                visible: btn.icon !== ""
                name: btn.icon
                size: 18
                color: btn.fg
                anchors.verticalCenter: parent.verticalCenter
            }

            StyledText {
                text: btn.label
                color: btn.fg
                font.pixelSize: Theme.fontSizeMedium
                font.weight: Font.Medium
                anchors.verticalCenter: parent.verticalCenter
            }
        }

        MouseArea {
            id: btnMouse
            anchors.fill: parent
            hoverEnabled: true
            cursorShape: Qt.PointingHandCursor
            onClicked: btn.clicked()
        }
    }

    // The shell's own button: its tooltip draws in an overlay, so the
    // scrolling list cannot clip it.
    component IconBtn: DActionButton {
        property string tip: ""

        iconSize: 18
        iconColor: Theme.surfaceVariantText
        buttonSize: 32
        tooltipText: tip !== "" ? tip : null
        tooltipSide: "top"
    }

    // Off | 10 min | On
    component VisibilitySwitch: Rectangle {
        id: vs

        readonly property int current: !root.visibleNow ? 0 : root.temporary ? 1 : 2
        readonly property var options: [
            { label: "Hidden", icon: "visibility_off", cmd: ["hidden"] },
            { label: "10 min", icon: "timer", cmd: ["temp", "10"] },
            { label: "Visible", icon: "visibility", cmd: ["visible"] }
        ]

        height: 40
        radius: height / 2
        color: Theme.surfaceContainerHigh
        opacity: root.up ? 1 : 0.5

        Rectangle {
            id: thumb
            width: (vs.width - 8) / 3
            height: vs.height - 8
            y: 4
            x: 4 + vs.current * width
            radius: height / 2
            color: vs.current === 0 ? Theme.surfaceContainerHighest : Theme.primary

            Behavior on x {
                NumberAnimation { duration: 180; easing.type: Easing.OutCubic }
            }
        }

        Row {
            x: 4
            y: 4

            Repeater {
                model: vs.options

                delegate: Item {
                    required property var modelData
                    required property int index
                    readonly property bool selected: index === vs.current
                    readonly property color fg: selected && index > 0 ? Theme.onPrimary : selected ? Theme.surfaceText : Theme.surfaceVariantText

                    width: (vs.width - 8) / 3
                    height: vs.height - 8

                    Row {
                        anchors.centerIn: parent
                        spacing: 6

                        DIcon {
                            name: modelData.icon
                            size: 18
                            color: parent.parent.fg
                            filled: parent.parent.selected
                            anchors.verticalCenter: parent.verticalCenter
                        }

                        StyledText {
                            text: modelData.label
                            font.pixelSize: Theme.fontSizeMedium
                            font.weight: parent.parent.selected ? Font.Medium : Font.Normal
                            color: parent.parent.fg
                            anchors.verticalCenter: parent.verticalCenter
                        }
                    }

                    MouseArea {
                        anchors.fill: parent
                        enabled: root.up
                        cursorShape: Qt.PointingHandCursor
                        onClicked: root.run(modelData.cmd)
                    }
                }
            }
        }
    }

    component RequestCard: Rectangle {
        id: rc

        property var t

        width: parent ? parent.width : 0
        height: rcCol.implicitHeight + Theme.spacingM * 2
        radius: Theme.cornerRadius * 1.5
        color: Theme.withAlpha(Theme.primary, 0.1)
        border.width: 1
        border.color: Theme.withAlpha(Theme.primary, 0.35)

        Column {
            id: rcCol
            x: Theme.spacingM
            y: Theme.spacingM
            width: parent.width - Theme.spacingM * 2
            spacing: Theme.spacingM

            Row {
                width: parent.width
                spacing: Theme.spacingM

                Rectangle {
                    width: 40
                    height: 40
                    radius: 20
                    color: Theme.primary

                    DIcon {
                        anchors.centerIn: parent
                        name: "smartphone"
                        size: 22
                        color: Theme.onPrimary
                    }
                }

                Column {
                    width: parent.width - 40 - pinChip.width - Theme.spacingM * 2
                    spacing: 2
                    anchors.verticalCenter: parent.verticalCenter

                    StyledText {
                        width: parent.width
                        text: rc.t.device + " wants to share"
                        font.pixelSize: Theme.fontSizeMedium
                        font.weight: Font.Medium
                        color: Theme.surfaceText
                        elide: Text.ElideRight
                        wrapMode: Text.NoWrap
                    }

                    StyledText {
                        width: parent.width
                        text: root.requestWhat(rc.t) + (rc.t.total_bytes > 0 ? " · " + root.fmtSize(rc.t.total_bytes) : "")
                        font.pixelSize: Theme.fontSizeSmall
                        color: Theme.surfaceVariantText
                        elide: Text.ElideMiddle
                        wrapMode: Text.NoWrap
                    }
                }

                Rectangle {
                    id: pinChip
                    visible: !!rc.t.pin
                    width: visible ? pinText.implicitWidth + 16 : 0
                    height: 24
                    radius: 12
                    color: Theme.surfaceContainerHighest
                    anchors.verticalCenter: parent.verticalCenter

                    StyledText {
                        id: pinText
                        anchors.centerIn: parent
                        text: "PIN " + (rc.t.pin || "")
                        font.pixelSize: Theme.fontSizeSmall
                        font.features: { "tnum": 1 }
                        color: Theme.surfaceVariantText
                    }
                }
            }

            Row {
                width: parent.width
                spacing: Theme.spacingS

                Btn {
                    variant: rc.t.kind === "files" ? "tonal" : "filled"
                    icon: "content_copy"
                    label: "Copy"
                    onClicked: root.run(["accept", rc.t.id, "copy"])
                }

                Btn {
                    variant: rc.t.kind === "files" ? "filled" : "tonal"
                    icon: "download"
                    label: "Save"
                    onClicked: root.run(["accept", rc.t.id, "save"])
                }

                Item {
                    width: Math.max(0, parent.width - x - declineBtn.width - parent.spacing)
                    height: 1
                }

                Btn {
                    id: declineBtn
                    variant: "text"
                    label: "Decline"
                    onClicked: root.run(["reject", rc.t.id])
                }
            }

            // One click to stop being asked for this phone.
            Item {
                width: parent.width
                height: 22

                Row {
                    id: trustLine
                    width: parent.width
                    spacing: 6
                    anchors.verticalCenter: parent.verticalCenter

                    DIcon {
                        name: "verified_user"
                        size: 16
                        color: trustMouse.containsMouse ? Theme.primary : Theme.surfaceVariantText
                        anchors.verticalCenter: parent.verticalCenter
                    }

                    StyledText {
                        // A long phone name ends in "…" instead of leaving the card.
                        width: Math.min(implicitWidth, trustLine.width - 22)
                        elide: Text.ElideRight
                        wrapMode: Text.NoWrap
                        text: "Always accept from " + rc.t.device
                        font.pixelSize: Theme.fontSizeSmall
                        font.underline: trustMouse.containsMouse
                        color: trustMouse.containsMouse ? Theme.primary : Theme.surfaceVariantText
                        anchors.verticalCenter: parent.verticalCenter
                    }
                }

                MouseArea {
                    id: trustMouse
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: {
                        root.run(["trust", rc.t.device]);
                        root.run(["accept", rc.t.id]);
                    }
                }
            }
        }
    }

    component ProgressCard: Rectangle {
        id: pc

        property var t
        readonly property real progress: Number(t.total_bytes) > 0 ? Math.min(1, Number(t.ack_bytes) / Number(t.total_bytes)) : 0

        width: parent ? parent.width : 0
        height: pcCol.implicitHeight + Theme.spacingM * 2
        radius: Theme.cornerRadius * 1.5
        color: Theme.surfaceContainerHigh

        Column {
            id: pcCol
            x: Theme.spacingM
            y: Theme.spacingM
            width: parent.width - Theme.spacingM * 2
            spacing: Theme.spacingS

            Item {
                width: parent.width
                height: 30

                DIcon {
                    id: pcIcon
                    name: pc.t.state === "finishing" ? "check_circle" : "download"
                    size: 22
                    color: Theme.primary
                    anchors.verticalCenter: parent.verticalCenter
                }

                Column {
                    anchors.left: pcIcon.right
                    anchors.leftMargin: Theme.spacingM
                    anchors.right: pcActions.left
                    anchors.rightMargin: Theme.spacingS
                    anchors.verticalCenter: parent.verticalCenter

                    StyledText {
                        width: parent.width
                        text: (pc.t.state === "finishing" ? "Finishing · " : "Receiving · ") + root.requestWhat(pc.t)
                        font.pixelSize: Theme.fontSizeMedium
                        color: Theme.surfaceText
                        elide: Text.ElideMiddle
                        wrapMode: Text.NoWrap
                    }

                    StyledText {
                        width: parent.width
                        text: "from " + pc.t.device + " · " + root.fmtSize(pc.t.ack_bytes) + " of " + root.fmtSize(pc.t.total_bytes)
                            + (pc.t.action === "copy" ? " · will copy" : pc.t.action === "save" ? " · will save" : "")
                        font.pixelSize: Theme.fontSizeSmall
                        color: Theme.surfaceVariantText
                        elide: Text.ElideRight
                        wrapMode: Text.NoWrap
                    }
                }

                IconBtn {
                    id: pcActions
                    visible: pc.t.state !== "finishing"
                    iconName: "close"
                    tip: "Cancel"
                    anchors.right: parent.right
                    anchors.verticalCenter: parent.verticalCenter
                    onClicked: root.run(["reject", pc.t.id])
                }
            }

            Rectangle {
                width: parent.width
                height: 6
                radius: 3
                color: Theme.withAlpha(Theme.primary, 0.18)

                Rectangle {
                    width: parent.width * pc.progress
                    height: parent.height
                    radius: parent.radius
                    color: Theme.primary

                    Behavior on width {
                        NumberAnimation { duration: 200 }
                    }
                }
            }
        }
    }

    // One received thing. Same shape as a row in the dictate plugin's lists.
    // Click opens it in its app (a link in the browser); text has nothing to
    // open, so a click copies it.
    component HistoryRow: Item {
        id: hr

        property var item
        readonly property bool isImage: item.kind === "image"
        readonly property bool hasFile: !!item.path
        readonly property bool opens: hasFile || item.kind === "url"
        readonly property bool hovered: rowHover.hovered

        width: parent ? parent.width : 0
        height: 54

        HoverHandler {
            id: rowHover
        }

        Rectangle {
            anchors.fill: parent
            radius: Theme.cornerRadius
            color: Theme.surfaceText
            opacity: rowMouse.pressed ? 0.1 : hr.hovered ? 0.05 : 0
        }

        MouseArea {
            id: rowMouse
            anchors.fill: parent
            cursorShape: Qt.PointingHandCursor
            onClicked: {
                if (hr.opens) {
                    root.run(["open", hr.item.id]);
                    root.closePopout();
                } else {
                    root.run(["copy", hr.item.id]);
                }
            }
        }

        // A photo shows itself; everything else is an icon in a circle.
        ClippingRectangle {
            id: thumbBox
            x: Theme.spacingS
            width: 36
            height: 36
            radius: thumbImg.visible ? 8 : 18
            color: Theme.surfaceContainerHigh
            anchors.verticalCenter: parent.verticalCenter

            Image {
                id: thumbImg
                anchors.fill: parent
                visible: hr.isImage && status === Image.Ready
                source: hr.isImage && hr.hasFile ? root.fileUrl(hr.item.path) : ""
                fillMode: Image.PreserveAspectCrop
                sourceSize.width: 96
                sourceSize.height: 96
                asynchronous: true
                autoTransform: true
                smooth: true
            }

            DIcon {
                anchors.centerIn: parent
                visible: !thumbImg.visible
                name: root.kindIcon(hr.item.kind)
                size: 18
                color: Theme.surfaceVariantText
            }
        }

        Column {
            anchors.left: thumbBox.right
            anchors.leftMargin: Theme.spacingM
            anchors.right: rowActions.visible ? rowActions.left : parent.right
            anchors.rightMargin: Theme.spacingS
            anchors.verticalCenter: parent.verticalCenter
            spacing: 2

            StyledText {
                width: parent.width
                text: hr.item.name || hr.item.text || ""
                textFormat: Text.PlainText
                font.pixelSize: Theme.fontSizeMedium
                color: Theme.surfaceText
                elide: Text.ElideMiddle
                wrapMode: Text.NoWrap
                maximumLineCount: 1
            }

            StyledText {
                width: parent.width
                text: root.subtitleOf(hr.item)
                font.pixelSize: Theme.fontSizeSmall
                font.features: { "tnum": 1 }
                color: Theme.surfaceVariantText
                elide: Text.ElideRight
                wrapMode: Text.NoWrap
            }
        }

        Row {
            id: rowActions
            visible: hr.hovered
            anchors.right: parent.right
            anchors.rightMargin: Theme.spacingXS
            anchors.verticalCenter: parent.verticalCenter

            IconBtn {
                iconName: "content_copy"
                tip: "Copy"
                onClicked: root.run(["copy", hr.item.id])
            }

            IconBtn {
                visible: !hr.item.saved
                iconName: "download"
                tip: "Save to Downloads"
                onClicked: root.run(["save", hr.item.id])
            }

            IconBtn {
                visible: hr.hasFile
                iconName: "folder_open"
                tip: "Show in folder"
                onClicked: {
                    root.run(["reveal", hr.item.id]);
                    root.closePopout();
                }
            }

            IconBtn {
                iconName: "delete"
                tip: hr.item.saved ? "Remove from the list" : "Delete"
                onClicked: root.run(["remove", hr.item.id])
            }
        }
    }

    // ---- popout ---------------------------------------------------------------

    popoutContent: Component {
        PopoutComponent {
            id: popout

            Column {
                width: parent.width
                topPadding: Theme.spacingS
                bottomPadding: Theme.spacingS
                spacing: Theme.spacingM

                // Header
                Item {
                    x: Theme.spacingS
                    width: parent.width - Theme.spacingS * 2
                    height: 44

                    Rectangle {
                        id: headIcon
                        width: 44
                        height: 44
                        radius: 22
                        color: root.visibleNow ? Theme.primary : Theme.surfaceContainerHigh

                        QsGlyph {
                            anchors.centerIn: parent
                            size: 24
                            slashed: !root.visibleNow
                            color: root.visibleNow ? Theme.onPrimary : (root.up ? Theme.surfaceVariantText : Theme.error)
                        }
                    }

                    Column {
                        anchors.left: headIcon.right
                        anchors.leftMargin: Theme.spacingM
                        anchors.right: parent.right
                        anchors.verticalCenter: parent.verticalCenter
                        spacing: 2

                        StyledText {
                            text: "Quick Share"
                            font.pixelSize: Theme.fontSizeLarge
                            font.weight: Font.Medium
                            color: Theme.surfaceText
                        }

                        StyledText {
                            width: parent.width
                            text: root.statusLine
                            font.pixelSize: Theme.fontSizeSmall
                            font.features: { "tnum": 1 }
                            color: root.up ? Theme.surfaceVariantText : Theme.error
                            elide: Text.ElideRight
                            wrapMode: Text.NoWrap
                        }
                    }
                }

                VisibilitySwitch {
                    x: Theme.spacingS
                    width: parent.width - Theme.spacingS * 2
                }

                // A phone is trying to share while we're hidden.
                Rectangle {
                    visible: root.nearby
                    x: Theme.spacingS
                    width: parent.width - Theme.spacingS * 2
                    height: 52
                    radius: Theme.cornerRadius * 1.5
                    color: Theme.withAlpha(Theme.warning, 0.14)

                    DIcon {
                        id: nearIcon
                        x: Theme.spacingM
                        name: "bluetooth_searching"
                        size: 20
                        color: Theme.warning
                        anchors.verticalCenter: parent.verticalCenter
                    }

                    StyledText {
                        anchors.left: nearIcon.right
                        anchors.leftMargin: Theme.spacingM
                        anchors.right: nearBtn.left
                        anchors.rightMargin: Theme.spacingS
                        anchors.verticalCenter: parent.verticalCenter
                        text: "A phone nearby is trying to share"
                        font.pixelSize: Theme.fontSizeSmall
                        color: Theme.surfaceText
                        wrapMode: Text.WordWrap
                    }

                    Btn {
                        id: nearBtn
                        anchors.right: parent.right
                        anchors.rightMargin: Theme.spacingS
                        anchors.verticalCenter: parent.verticalCenter
                        variant: "filled"
                        label: "Show me"
                        onClicked: root.run(["temp", "10"])
                    }
                }

                Column {
                    visible: root.waiting.length > 0 || root.activeIds.length > 0
                    x: Theme.spacingS
                    width: parent.width - Theme.spacingS * 2
                    spacing: Theme.spacingS

                    Repeater {
                        model: root.waiting

                        delegate: RequestCard {
                            required property var modelData
                            t: modelData
                        }
                    }

                    Repeater {
                        model: root.activeIds

                        delegate: ProgressCard {
                            required property var modelData
                            t: root.byId[modelData] || ({ files: [] })
                        }
                    }
                }

                Rectangle {
                    x: Theme.spacingS
                    width: parent.width - Theme.spacingS * 2
                    height: 1
                    color: Theme.withAlpha(Theme.outline, 0.2)
                }

                // Recent: title, the one spot for messages, Open Downloads, Delete all
                // ("Delete all?" asks first, in the same row).
                Item {
                    id: recentHead
                    visible: root.up
                    x: Theme.spacingS
                    width: parent.width - Theme.spacingS * 2
                    height: 32

                    StyledText {
                        id: recentTitle
                        visible: !root.confirmClear
                        anchors.verticalCenter: parent.verticalCenter
                        text: "Recent"
                        font.pixelSize: Theme.fontSizeMedium
                        font.weight: Font.Medium
                        color: Theme.surfaceVariantText
                    }

                    // A word from the daemon or about a click: green for "Copied",
                    // "Saved to Downloads", red when something went wrong. It fades in
                    // place (the two buttons step aside), so nothing moves.
                    Rectangle {
                        id: statusChip
                        readonly property string say: root.note !== "" ? root.note : root.lastNote[0]
                        readonly property bool bad: root.note !== "" ? root.noteBad : root.lastNote[1]
                        readonly property color ink: bad ? Theme.error : Theme.success
                        readonly property real room: recentHead.width - recentTitle.width - Theme.spacingS
                        visible: !root.confirmClear && opacity > 0
                        anchors.right: parent.right
                        anchors.verticalCenter: parent.verticalCenter
                        // Never wider than the space next to the title: a long text ends in "…".
                        width: Math.min(statusRow.implicitWidth + Theme.spacingM * 2, room)
                        height: 26
                        radius: 13
                        color: Theme.withAlpha(ink, 0.15)
                        opacity: root.note !== "" ? 1 : 0
                        Behavior on opacity {
                            NumberAnimation { duration: 150 }
                        }

                        Row {
                            id: statusRow
                            x: Theme.spacingM
                            anchors.verticalCenter: parent.verticalCenter
                            spacing: 6

                            DIcon {
                                name: statusChip.bad ? "error" : "check_circle"
                                size: 16
                                color: statusChip.ink
                                anchors.verticalCenter: parent.verticalCenter
                            }

                            StyledText {
                                width: Math.min(implicitWidth, statusChip.room - Theme.spacingM * 2 - 22)
                                text: statusChip.say
                                textFormat: Text.PlainText
                                font.pixelSize: Theme.fontSizeSmall
                                color: Theme.surfaceText
                                elide: Text.ElideRight
                                wrapMode: Text.NoWrap
                                anchors.verticalCenter: parent.verticalCenter
                            }
                        }
                    }

                    Row {
                        visible: !root.confirmClear && root.note === ""
                        anchors.right: parent.right
                        anchors.verticalCenter: parent.verticalCenter

                        IconBtn {
                            iconName: "folder"
                            tip: "Open Downloads"
                            onClicked: {
                                root.run(["downloads"]);
                                root.closePopout();
                            }
                        }

                        IconBtn {
                            visible: root.history.length > 0
                            iconName: "delete_sweep"
                            tip: "Delete all (saved files stay in Downloads)"
                            onClicked: {
                                root.confirmClear = true;
                                confirmTimer.restart();
                            }
                        }
                    }

                    Timer {
                        id: confirmTimer
                        interval: 5000
                        onTriggered: root.confirmClear = false
                    }

                    Item {
                        visible: root.confirmClear
                        anchors.fill: parent

                        Row {
                            anchors.verticalCenter: parent.verticalCenter
                            spacing: 8

                            DIcon {
                                name: "delete_sweep"
                                size: 18
                                color: Theme.error
                                anchors.verticalCenter: parent.verticalCenter
                            }

                            StyledText {
                                text: "Delete all " + root.history.length + "?"
                                font.pixelSize: Theme.fontSizeMedium
                                font.weight: Font.Medium
                                color: Theme.surfaceText
                                anchors.verticalCenter: parent.verticalCenter
                            }
                        }

                        Row {
                            anchors.right: parent.right
                            anchors.verticalCenter: parent.verticalCenter
                            spacing: 6

                            Rectangle {
                                width: 80
                                height: 28
                                radius: 14
                                color: keepArea.containsMouse ? Theme.withAlpha(Theme.surfaceText, 0.08) : "transparent"

                                StyledText {
                                    anchors.centerIn: parent
                                    text: "Cancel"
                                    font.pixelSize: Theme.fontSizeMedium
                                    font.weight: Font.Medium
                                    color: Theme.surfaceVariantText
                                }

                                MouseArea {
                                    id: keepArea
                                    anchors.fill: parent
                                    hoverEnabled: true
                                    cursorShape: Qt.PointingHandCursor
                                    onClicked: root.confirmClear = false
                                }
                            }

                            Rectangle {
                                width: 76
                                height: 28
                                radius: 14
                                color: delArea.containsMouse ? Theme.withAlpha(Theme.error, 0.26) : Theme.withAlpha(Theme.error, 0.16)

                                StyledText {
                                    anchors.centerIn: parent
                                    text: "Delete"
                                    font.pixelSize: Theme.fontSizeMedium
                                    font.weight: Font.Medium
                                    color: Theme.error
                                }

                                MouseArea {
                                    id: delArea
                                    anchors.fill: parent
                                    hoverEnabled: true
                                    cursorShape: Qt.PointingHandCursor
                                    onClicked: {
                                        root.confirmClear = false;
                                        root.showAll = false;
                                        root.run(["clear"]);
                                    }
                                }
                            }
                        }
                    }
                }

                // The newest 5, or all of them with a scroll after "Show N more".
                Flickable {
                    id: listFlick
                    visible: root.history.length > 0
                    x: Theme.spacingS
                    width: parent.width - Theme.spacingS * 2
                    height: Math.min(historyCol.implicitHeight, 54 * (root.showAll ? 7 : 5))
                    contentWidth: width
                    contentHeight: historyCol.implicitHeight
                    clip: true
                    boundsBehavior: Flickable.StopAtBounds
                    interactive: contentHeight > height

                    Column {
                        id: historyCol
                        width: listFlick.width

                        Repeater {
                            model: root.showAll ? root.history : root.history.slice(0, 5)

                            delegate: HistoryRow {
                                required property var modelData
                                item: modelData
                            }
                        }
                    }
                }

                Rectangle {
                    visible: root.history.length > 5
                    x: Theme.spacingS
                    width: parent.width - Theme.spacingS * 2
                    height: 34
                    radius: Theme.cornerRadius
                    color: moreArea.containsMouse ? Theme.withAlpha(Theme.primary, 0.08) : "transparent"

                    StyledText {
                        anchors.centerIn: parent
                        text: root.showAll ? "Show less" : "Show " + (root.history.length - 5) + " more"
                        font.pixelSize: Theme.fontSizeMedium
                        font.weight: Font.Medium
                        color: Theme.primary
                    }

                    MouseArea {
                        id: moreArea
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: root.showAll = !root.showAll
                    }
                }

                // Empty state
                Column {
                    visible: root.up && root.history.length === 0
                    width: parent.width
                    spacing: Theme.spacingXS
                    topPadding: Theme.spacingS
                    bottomPadding: Theme.spacingS

                    DIcon {
                        anchors.horizontalCenter: parent.horizontalCenter
                        name: "send_to_mobile"
                        size: 32
                        color: Theme.withAlpha(Theme.surfaceVariantText, 0.6)
                    }

                    StyledText {
                        anchors.horizontalCenter: parent.horizontalCenter
                        text: "Nothing received yet"
                        font.pixelSize: Theme.fontSizeMedium
                        color: Theme.surfaceText
                    }

                    StyledText {
                        width: parent.width - Theme.spacingL * 2
                        anchors.horizontalCenter: parent.horizontalCenter
                        horizontalAlignment: Text.AlignHCenter
                        wrapMode: Text.WordWrap
                        text: "On your phone: Share → Quick Share → " + root.deviceName + ". Photos land on the clipboard."
                        font.pixelSize: Theme.fontSizeSmall
                        color: Theme.surfaceVariantText
                    }
                }

                // Not running
                Column {
                    visible: !root.up
                    width: parent.width
                    spacing: Theme.spacingS

                    StyledText {
                        width: parent.width - Theme.spacingL * 2
                        anchors.horizontalCenter: parent.horizontalCenter
                        horizontalAlignment: Text.AlignHCenter
                        wrapMode: Text.WordWrap
                        text: "Start it with: systemctl --user start qsbar"
                        font.pixelSize: Theme.fontSizeSmall
                        color: Theme.surfaceVariantText
                    }

                    Btn {
                        anchors.horizontalCenter: parent.horizontalCenter
                        variant: "filled"
                        icon: "play_arrow"
                        label: "Start receiver"
                        onClicked: Quickshell.execDetached(["systemctl", "--user", "start", "qsbar.service"])
                    }
                }

                // Trusted phones
                Flow {
                    visible: root.up && root.trusted.length > 0
                    x: Theme.spacingM
                    width: parent.width - Theme.spacingM * 2
                    spacing: Theme.spacingXS

                    StyledText {
                        height: 26
                        verticalAlignment: Text.AlignVCenter
                        text: "Auto-accept:"
                        font.pixelSize: Theme.fontSizeSmall
                        color: Theme.surfaceVariantText
                    }

                    Repeater {
                        model: root.trusted

                        delegate: Rectangle {
                            required property var modelData

                            width: trustRow.implicitWidth + 14
                            height: 26
                            radius: 13
                            color: Theme.surfaceContainerHigh

                            Row {
                                id: trustRow
                                anchors.centerIn: parent
                                spacing: 4

                                StyledText {
                                    // A long phone name ends in "…".
                                    width: Math.min(implicitWidth, 220)
                                    elide: Text.ElideRight
                                    wrapMode: Text.NoWrap
                                    text: modelData
                                    font.pixelSize: Theme.fontSizeSmall
                                    color: Theme.surfaceText
                                    anchors.verticalCenter: parent.verticalCenter
                                }

                                DIcon {
                                    name: "close"
                                    size: 14
                                    color: untrustMouse.containsMouse ? Theme.error : Theme.surfaceVariantText
                                    anchors.verticalCenter: parent.verticalCenter

                                    MouseArea {
                                        id: untrustMouse
                                        anchors.fill: parent
                                        anchors.margins: -4
                                        hoverEnabled: true
                                        cursorShape: Qt.PointingHandCursor
                                        onClicked: root.run(["untrust", modelData])
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    popoutWidth: 400
}
