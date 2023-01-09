use crate::encryption::decrypt_message;
use crate::messenger::Message;
use crate::task_runner::Task;
use anyhow::Error;
use anyhow::Result;
use serde_json::Value;

pub fn convert_message_to_task(msg: Message, cek: &[u8]) -> Result<Task> {
    let cmd = String::from_utf8(decrypt_message(msg.content, cek)?)?;

    if let Value::Object(obj) = serde_json::from_str(&cmd)? {
        let name = obj
            .get("task")
            .ok_or_else(|| Error::msg("Invalid task"))?
            .as_str()
            .unwrap()
            .to_string();

        let args = obj
            .get("args")
            .and_then(|v| v.as_str().map(|v| v.to_string()))
            .unwrap_or_default();

        let secret = obj
            .get("secret")
            .and_then(|v| v.as_str().map(|v| v.to_string()))
            .unwrap_or_default();

        let action = obj
            .get("action")
            .and_then(|v| v.as_str().map(|v| v.to_string()))
            .unwrap_or_default();

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

    let task = convert_message_to_task(
        Message {
            id: 1,
            sender: Default::default(),
            created_at: 0,
            flags: 1,
            content: "5Pao+b7gUXFli77uhNwzjQZN4qA6PyYqra8ySFjTKDSUGGLkqBeUgKBSAqvkE5rLryKDHP9DPWjfRYu59Ge5J0IkgepJ6rAGI8HZdlnekmUIfjCo8ZrH/3eq/pHquCX1wB6d".to_string(),
        },
        cek,
    ).unwrap();

    assert_eq!(task.id, 1);
    assert_eq!(task.name, "test");
    assert_eq!(task.args, "sleep=60");
    assert_eq!(task.secret, "secret123");
}