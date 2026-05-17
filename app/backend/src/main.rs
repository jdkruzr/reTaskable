use appload_client::{
    AppLoad, AppLoadBackend, BackendReplier, Message, MSG_SYSTEM_NEW_COORDINATOR,
};
use async_trait::async_trait;

const MSG_PING: u32 = 1;
const MSG_PONG: u32 = 101;

#[tokio::main]
async fn main() {
    AppLoad::new(Backend).unwrap().run().await.unwrap();
}

struct Backend;

#[async_trait]
impl AppLoadBackend for Backend {
    async fn handle_message(&mut self, replier: &BackendReplier<Self>, msg: Message) {
        match msg.msg_type {
            MSG_SYSTEM_NEW_COORDINATOR => {
                eprintln!("retaskable: frontend connected");
            }
            MSG_PING => {
                eprintln!("retaskable: ping received ({:?})", msg.contents);
                if let Err(e) = replier.send_message(MSG_PONG, "pong from reTaskable") {
                    eprintln!("retaskable: send failed: {e}");
                }
            }
            t => eprintln!("retaskable: ignoring unknown msg type {t}"),
        }
    }
}
