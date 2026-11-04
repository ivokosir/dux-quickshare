//! Test sender: pushes files to a Quick Share receiver at ADDR (ip:port).
//! cargo run --release --example send -- 127.0.0.1:PORT file...
use rqs_lib::channel::ChannelDirection;
use rqs_lib::{OutboundPayload, SendInfo, State, Visibility, RQS};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let addr = args.first().cloned().expect("addr");
    let files: Vec<String> = args[1..]
        .iter()
        .map(|f| std::fs::canonicalize(f).unwrap().to_string_lossy().to_string())
        .collect();

    let mut rqs = RQS::new(Visibility::Invisible, None, None);
    let (tx, _) = rqs.run().await?;
    let mut rx = rqs.message_sender.subscribe();
    tx.send(SendInfo { id: addr.clone(), name: "Test Sender".into(), addr, ob: OutboundPayload::Files(files) })
        .await?;

    let result = tokio::time::timeout(std::time::Duration::from_secs(120), async {
        while let Ok(msg) = rx.recv().await {
            if msg.direction != ChannelDirection::LibToFront {
                continue;
            }
            if let Some(state) = msg.state {
                println!("sender state: {state:?}");
                if matches!(state, State::Finished | State::Rejected | State::Cancelled | State::Disconnected) {
                    return state;
                }
            }
        }
        State::Disconnected
    })
    .await;
    println!("result: {result:?}");
    rqs.stop().await;
    Ok(())
}
