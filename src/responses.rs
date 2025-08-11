use serde::{Deserialize, Serialize};
use uuid::Uuid;
use crate::types::{Role, Metadata};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseRequest {
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<InputTypes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_response_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub store: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Metadata>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub safety_identifier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ResponseTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verbosity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncation: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum InputTypes {
    Text(String),
    Array(Vec<InputItem>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputItem {
    #[serde(rename = "type")]
    pub item_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<Role>,
    #[serde(flatten)]
    pub content: InputContent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum InputContent {
    Text { text: String },
    Image { 
        image_url: ImageUrl,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    File {
        file_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        purpose: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageUrl {
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseTool {
    #[serde(rename = "type")]
    pub tool_type: String,
    #[serde(flatten)]
    pub tool_data: ToolData,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolData {
    Function {
        function: FunctionTool,
    },
    WebSearch {
        web_search: WebSearchTool,
    },
    FileSearch {
        file_search: FileSearchTool,
    },
    CodeInterpreter {
        code_interpreter: CodeInterpreterTool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionTool {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSearchTool {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSearchTool {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_ids: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeInterpreterTool {}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolChoice {
    Auto,
    None,
    Required,
    Function { function: FunctionChoice },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionChoice {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseObject {
    pub id: String,
    pub object: String,
    pub created_at: i64,
    pub model: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_response_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub store: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Metadata>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub safety_identifier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ResponseTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,
    pub output: Vec<OutputItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ResponseError>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub incomplete_details: Option<IncompleteDetails>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<Reasoning>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verbosity: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputItem {
    pub id: String,
    #[serde(rename = "type")]
    pub item_type: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<Role>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<OutputContent>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputContent {
    #[serde(rename = "type")]
    pub content_type: String,
    #[serde(flatten)]
    pub content_data: OutputContentData,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OutputContentData {
    Text {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        annotations: Option<Vec<serde_json::Value>>,
    },
    ToolCall {
        id: String,
        function: ToolCallFunction,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallFunction {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseError {
    #[serde(rename = "type")]
    pub error_type: String,
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub param: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncompleteDetails {
    #[serde(rename = "type")]
    pub incomplete_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens_details: Option<TokenDetails>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens_details: Option<TokenDetails>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reasoning {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encrypted_content: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteResponseResult {
    pub id: String,
    pub object: String,
    pub deleted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputItemList {
    pub object: String,
    pub data: Vec<InputItem>,
    pub first_id: String,
    pub last_id: String,
    pub has_more: bool,
}

impl ResponseRequest {
    pub fn generate_id() -> String {
        format!("resp_{}", Uuid::new_v4().simple())
    }

    pub fn generate_message_id() -> String {
        format!("msg_{}", Uuid::new_v4().simple())
    }

    pub fn to_chat_completion_request(&self, conversation_history: Vec<endpoints::chat::ChatCompletionRequestMessage>) -> endpoints::chat::ChatCompletionRequest {
        let mut messages = Vec::new();

        // Add conversation history first
        messages.extend(conversation_history);

        // Add current instructions as system message if present
        if let Some(instructions) = &self.instructions {
            messages.push(endpoints::chat::ChatCompletionRequestMessage::System(
                endpoints::chat::ChatCompletionSystemMessage::new(instructions.clone(), None)
            ));
        }

        // Convert input to user messages
        if let Some(input) = &self.input {
            match input {
                InputTypes::Text(text) => {
                    messages.push(endpoints::chat::ChatCompletionRequestMessage::User(
                        endpoints::chat::ChatCompletionUserMessage::new(
                            endpoints::chat::ChatCompletionUserMessageContent::Text(text.clone()),
                            None
                        )
                    ));
                }
                InputTypes::Array(items) => {
                    for item in items {
                        // Convert input items to appropriate message types
                        match &item.content {
                            InputContent::Text { text } => {
                                let role = item.role.as_ref().unwrap_or(&Role::User);
                                match role {
                                    Role::User => {
                                        messages.push(endpoints::chat::ChatCompletionRequestMessage::User(
                                            endpoints::chat::ChatCompletionUserMessage::new(
                                                endpoints::chat::ChatCompletionUserMessageContent::Text(text.clone()),
                                                None
                                            )
                                        ));
                                    }
                                    Role::System => {
                                        messages.push(endpoints::chat::ChatCompletionRequestMessage::System(
                                            endpoints::chat::ChatCompletionSystemMessage::new(text.clone(), None)
                                        ));
                                    }
                                    _ => {
                                        // Default to user message
                                        messages.push(endpoints::chat::ChatCompletionRequestMessage::User(
                                            endpoints::chat::ChatCompletionUserMessage::new(
                                                endpoints::chat::ChatCompletionUserMessageContent::Text(text.clone()),
                                                None
                                            )
                                        ));
                                    }
                                }
                            }
                            _ => {
                                // For now, skip non-text inputs
                                // TODO: Implement image and file support
                            }
                        }
                    }
                }
            }
        }

        // Convert tools
        let tools = self.tools.as_ref().map(|response_tools| {
            response_tools.iter().filter_map(|tool| {
                match &tool.tool_data {
                    ToolData::Function { function } => {
                        Some(endpoints::chat::Tool::new(endpoints::chat::ToolFunction {
                            name: function.name.clone(),
                            description: function.description.clone(),
                            parameters: function.parameters.as_ref().and_then(|p| p.as_object().cloned()),
                        }))
                    }
                    _ => None, // Skip non-function tools for now
                }
            }).collect()
        });

        // Convert tool choice
        let tool_choice = self.tool_choice.as_ref().map(|choice| {
            match choice {
                ToolChoice::Auto => endpoints::chat::ToolChoice::Auto,
                ToolChoice::None => endpoints::chat::ToolChoice::None,
                ToolChoice::Required => endpoints::chat::ToolChoice::Required,
                ToolChoice::Function { .. } => {
                    // For now, default to Auto when Function is specified
                    // TODO: Implement proper function tool choice support
                    endpoints::chat::ToolChoice::Auto
                }
            }
        });

        endpoints::chat::ChatCompletionRequest {
            model: Some(self.model.clone()),
            messages,
            temperature: self.temperature,
            top_p: self.top_p,
            max_completion_tokens: self.max_output_tokens.map(|t| t as i32),
            stream: self.stream,
            tools,
            tool_choice,
            user: self.user.clone(),
            ..Default::default()
        }
    }
}

impl From<endpoints::chat::ChatCompletionObject> for ResponseObject {
    fn from(completion: endpoints::chat::ChatCompletionObject) -> Self {
        let output = completion.choices.into_iter().map(|choice| {
            let content = vec![OutputContent {
                content_type: "output_text".to_string(),
                content_data: OutputContentData::Text {
                    text: choice.message.content.unwrap_or_default(),
                    annotations: None,
                },
            }];

            OutputItem {
                id: ResponseRequest::generate_message_id(),
                item_type: "message".to_string(),
                status: "completed".to_string(),
                role: Some(Role::Assistant),
                content: Some(content),
            }
        }).collect();

        ResponseObject {
            id: completion.id,
            object: "response".to_string(),
            created_at: completion.created as i64,
            model: completion.model,
            status: "completed".to_string(),
            previous_response_id: None,
            instructions: None,
            max_output_tokens: None,
            temperature: None,
            top_p: None,
            store: Some(true),
            metadata: None,
            user: None,
            safety_identifier: None,
            prompt_cache_key: None,
            tools: None,
            tool_choice: None,
            parallel_tool_calls: None,
            output,
            error: None,
            incomplete_details: None,
            usage: Some(Usage {
                input_tokens: completion.usage.prompt_tokens as i64,
                output_tokens: completion.usage.completion_tokens as i64,
                total_tokens: completion.usage.total_tokens as i64,
                input_tokens_details: None,
                output_tokens_details: None,
            }),
            reasoning: None,
            truncation: None,
            verbosity: None,
        }
    }
}