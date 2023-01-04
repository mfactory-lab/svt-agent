use crate::encryption::decrypt_message;
use crate::runner::Task;
use crate::state::Message;
use anyhow::Error;
use anyhow::Result;
use serde_json::Value;

pub fn convert_message_to_task(msg: Message, cek: &[u8]) -> Result<Task> {
    let cmd = String::from_utf8(decrypt_message(msg.content, cek)?)?;

    if let Value::Object(obj) = serde_json::from_str(&cmd)? {
        if !obj.contains_key("task") {
            return Err(Error::msg("task required"));
        }

        let name = obj["task"].as_str().unwrap_or_default().to_string();
        let args = obj["args"].as_str().unwrap_or_default().to_string();
        let secret = obj["secret"].as_str().unwrap_or_default().to_string();
        let action = obj["action"].as_str().unwrap_or_default().to_string();

        return Ok(Task {
            id: msg.id,
            name,
            args,
            secret,
            action,
        });
    }

    Err(Error::msg("Invalid task"))
}

#[test]
fn test() {
    let cek = &[
        101, 97, 28, 54, 107, 211, 170, 65, 21, 43, 117, 236, 75, 150, 33, 199, 74, 130, 91, 52,
        29, 138, 30, 252, 180, 224, 30, 81, 115, 178, 213, 204,
    ];
    let task = convert_message_to_task(
        Message {
            id: 1,
            sender: Default::default(),
            created_at: 0,
            flags: 1,
            content: "/MJ//PmiZkuIMa3z9pU8ksvE/MvzHpvp8A++VD9L2ZLB5V4PkuHnydi6bgqh1gZ/mih+NogvRxY2m0k23zwYzbKb2C4dGiPVmHt6ufOQtT6z9l6sahqp1LrlCDO0WtIEWNsvXSaoGAP4d03r8V1NDQn68acJDMA=".to_string(),
        },
        cek,
    ).unwrap();

    assert_eq!(task.id, 1);
    assert_eq!(task.name, "restart");
    assert_eq!(task.args, "a=1 b=2");
    assert_eq!(task.secret, "secret123");
    assert_eq!(task.action, "skip");
}
