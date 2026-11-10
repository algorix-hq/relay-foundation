use serde::{Deserialize, Serialize};

use crate::{AssistantContent, TextPart, UserContent};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case", deny_unknown_fields)]
pub enum Message {
    System { content: Vec<TextPart> },
    Developer { content: Vec<TextPart> },
    User { content: Vec<UserContent> },
    Assistant { content: Vec<AssistantContent> },
}
