# OpenAI /responses API Implementation in Llama Nexus

## Overview

Llama Nexus now supports OpenAI's **stateful `/responses` API** in addition to the existing stateless `/chat/completions` API.

### Key Components

1. **Request Processing**: Convert OpenAI `/responses` format to `/chat/completions`
2. **State Management**: Store conversation history and session data in SQLite database
3. **Response Conversion**: Transform LLM responses back to OpenAI `/responses` format
4. **Conversation Threading**: Link responses using `previous_response_id` for multi-turn conversations

## Database Schema

### Core Tables

```sql
-- Main response sessions (matches OpenAI ResponseObject)
CREATE TABLE responses (
    id TEXT PRIMARY KEY,                    -- "resp_abc123" format
    object TEXT DEFAULT 'response',         -- Always "response"
    created_at INTEGER NOT NULL,            -- Unix timestamp
    status TEXT NOT NULL,                   -- "completed", "in_progress", "failed"
    model TEXT NOT NULL,                    -- Model used
    previous_response_id TEXT,              -- Conversation chaining
    instructions TEXT,                      -- System instructions
    max_output_tokens INTEGER,
    temperature REAL,
    top_p REAL,
    store BOOLEAN DEFAULT TRUE,
    metadata TEXT,                          -- JSON string
    user_id TEXT,
    safety_identifier TEXT,
    prompt_cache_key TEXT,
    usage_input_tokens INTEGER,
    usage_output_tokens INTEGER,
    usage_total_tokens INTEGER,
    error TEXT,                             -- JSON string if error
    incomplete_details TEXT,                -- JSON string
    FOREIGN KEY (previous_response_id) REFERENCES responses(id)
);

-- Input items (what user sent)
CREATE TABLE input_items (
    id TEXT PRIMARY KEY,                    -- "msg_xyz789"
    response_id TEXT NOT NULL,              -- Links to parent response
    item_type TEXT NOT NULL,                -- "message", "file", "image"
    role TEXT,                              -- "user", "assistant", "system"
    content TEXT NOT NULL,                  -- JSON serialized content
    created_at INTEGER NOT NULL,
    FOREIGN KEY (response_id) REFERENCES responses(id) ON DELETE CASCADE
);

-- Output items (what AI responded)
CREATE TABLE output_items (
    id TEXT PRIMARY KEY,                    -- "msg_def456"  
    response_id TEXT NOT NULL,              -- Links to parent response
    item_type TEXT NOT NULL,                -- "message", "tool_call"
    role TEXT,                              -- Usually "assistant"
    content TEXT NOT NULL,                  -- JSON serialized content
    status TEXT NOT NULL,                   -- "completed", "failed"
    created_at INTEGER NOT NULL,
    FOREIGN KEY (response_id) REFERENCES responses(id) ON DELETE CASCADE
);
```

### Why This Schema?

- **OpenAI Compliant**: Directly maps to OpenAI's ResponseObject structure
- **Multimodal Support**: Handles text, images, files, and tool calls
- **Conversation Threading**: Uses `previous_response_id` for proper chaining
- **Normalized**: Separates inputs/outputs for complex scenarios
- **JSON Flexibility**: Stores complex content as JSON for future extensibility

## API Endpoints

### Create Response
```http
POST /v1/responses
Content-Type: application/json

{
  "model": "gpt-4",
  "input": "Tell me about quantum computing",
  "temperature": 0.7,
  "max_output_tokens": 1000
}
```

**Response:**
```json
{
  "id": "resp_67ccd2bed1ec8190b14f964abc0542670bb6a6b452d3795b",
  "object": "response",
  "created_at": 1741476542,
  "status": "completed",
  "model": "gpt-4",
  "output": [
    {
      "id": "msg_67ccd2bf17f0819081ff3bb2cf6508e60bb6a6b452d3795b",
      "type": "message",
      "status": "completed",
      "role": "assistant",
      "content": [
        {
          "type": "output_text",
          "text": "Quantum computing is a revolutionary computing paradigm..."
        }
      ]
    }
  ],
  "usage": {
    "input_tokens": 36,
    "output_tokens": 87,
    "total_tokens": 123
  }
}
```

### Multi-turn Conversation
```http
POST /v1/responses
Content-Type: application/json

{
  "model": "gpt-4",
  "input": "Can you elaborate on quantum entanglement?",
  "previous_response_id": "resp_67ccd2bed1ec8190b14f964abc0542670bb6a6b452d3795b"
}
```

### Get Response
```http
GET /v1/responses/{response_id}
```

### Delete Response
```http
DELETE /v1/responses/{response_id}
```

### List Input Items
```http
GET /v1/responses/{response_id}/input_items
```

## Implementation Details

### 1. Request Processing Flow

```rust
// 1. Receive OpenAI /responses request
pub struct ResponseRequest {
    pub model: String,
    pub input: Option<InputTypes>,
    pub instructions: Option<String>,
    pub previous_response_id: Option<String>,
    pub temperature: Option<f64>,
    // ... other OpenAI fields
}

// 2. Build conversation history from database
let conversation_history = if let Some(prev_id) = &request.previous_response_id {
    build_conversation_history(&database, prev_id).await?
} else {
    Vec::new()
};

// 3. Convert to ChatCompletionRequest
let chat_request = request.to_chat_completion_request(conversation_history);

// 4. Send to downstream LLM server
let response = send_to_downstream_server(chat_request).await?;

// 5. Convert back to OpenAI ResponseObject
let response_obj = ResponseObject::from(response);

// 6. Store in database
database.store_response(response_obj).await?;
```

### 2. Type Safety Features

**Role Enum:**
```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
    System,
}
```

**Metadata Validation:**
```rust
#[derive(Debug, Clone, Serialize, PartialEq, Eq, Default)]
pub struct Metadata(HashMap<String, String>);

impl Metadata {
    // Enforces OpenAI constraints:
    // - Max 16 key-value pairs
    // - Keys max 64 characters
    // - Values max 512 characters
    pub fn from_map(map: HashMap<String, String>) -> Result<Self, MetadataError> {
        // Validation logic...
    }
}
```

### 3. Conversation Management

**Building Conversation History:**
```rust
async fn build_conversation_history(
    database: &DatabaseManager,
    previous_response_id: String,
) -> Result<Vec<ChatCompletionRequestMessage>> {
    // 1. Get response chain via previous_response_id links
    let response_sessions = database.get_conversation_history(&previous_response_id).await?;
    
    // 2. For each response, get input and output items
    for response in response_sessions {
        // Add system instructions
        if let Some(instructions) = &response.instructions {
            messages.push(system_message(instructions));
        }
        
        // Add user inputs
        let input_items = database.get_input_items(&response.id).await?;
        for item in input_items {
            messages.push(convert_input_to_message(item));
        }
        
        // Add assistant outputs  
        let output_items = database.get_output_items(&response.id).await?;
        for item in output_items {
            messages.push(convert_output_to_message(item));
        }
    }
    
    Ok(messages)
}
```