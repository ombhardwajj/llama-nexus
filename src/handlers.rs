use std::{sync::Arc, time::SystemTime};

use axum::{
    Json,
    body::Body,
    extract::{Extension, State},
    http::{HeaderMap, Response, StatusCode},
};
use bytes::Bytes;
use endpoints::{
    chat::{
        ChatCompletionAssistantMessage, ChatCompletionChunk, ChatCompletionObject,
        ChatCompletionRequest, ChatCompletionRequestMessage, ChatCompletionToolMessage, Tool,
        ToolCall, ToolChoice, ToolFunction,
    },
    embeddings::EmbeddingRequest,
    models::{ListModelsResponse, Model},
};
use futures_util::StreamExt;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use rmcp::model::{CallToolRequestParam, RawContent};
use tokio::select;
use tokio_util::sync::CancellationToken;

use crate::{
    AppState, dual_debug, dual_error, dual_info, dual_warn,
    error::{ServerError, ServerResult},
    info::ApiServer,
    mcp::{DEFAULT_SEARCH_FALLBACK_MESSAGE, MCP_SERVICES, MCP_TOOLS, SEARCH_MCP_SERVER_NAMES},
    server::{RoutingPolicy, Server, ServerIdToRemove, ServerKind, TargetServerInfo},
    types::Role,
};

pub(crate) async fn chat_handler(
    State(state): State<Arc<AppState>>,
    Extension(cancel_token): Extension<CancellationToken>,
    headers: HeaderMap,
    Json(mut request): Json<ChatCompletionRequest>,
) -> ServerResult<axum::response::Response> {
    let request_id = headers
        .get("x-request-id")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("unknown")
        .to_string();

    // check if the user id is provided
    if request.user.is_none() {
        request.user = Some(gen_chat_id());
    };
    dual_info!(
        "Received a new chat request from user: {} - request_id: {}",
        request.user.as_ref().unwrap(),
        request_id
    );

    // update the request with MCP tools
    dual_info!("Updating the request with MCP tools");
    if let Some(mcp_config) = state.config.read().await.mcp.as_ref()
        && !mcp_config.server.tool_servers.is_empty()
    {
        let mut more_tools = Vec::new();
        for server_config in mcp_config.server.tool_servers.iter() {
            if server_config.enable {
                server_config
                    .tools
                    .as_ref()
                    .unwrap()
                    .iter()
                    .for_each(|mcp_tool| {
                        let tool = Tool::new(ToolFunction {
                            name: mcp_tool.name.to_string(),
                            description: mcp_tool.description.as_ref().map(|s| s.to_string()),
                            parameters: Some((*mcp_tool.input_schema).clone()),
                        });

                        more_tools.push(tool.clone());
                    });
            }
        }

        if !more_tools.is_empty() {
            if let Some(tools) = &mut request.tools {
                tools.extend(more_tools);
            } else {
                request.tools = Some(more_tools);
            }

            // set the tool choice to auto
            if let Some(ToolChoice::None) | None = request.tool_choice {
                request.tool_choice = Some(ToolChoice::Auto);
            }
        }
    }

    // ! DO NOT REMOVE THIS BLOCK
    {
        // let enable_rag = state.config.read().await.rag.as_ref().unwrap().enable;
        // match enable_rag {
        //     true => {
        //         rag::chat(
        //             State(state),
        //             Extension(cancel_token),
        //             headers,
        //             Json(request),
        //             &request_id,
        //         )
        //         .await
        //     }
        //     false => {
        //         chat(
        //             State(state),
        //             Extension(cancel_token),
        //             headers,
        //             Json(request),
        //             &request_id,
        //         )
        //         .await
        //     }
        // }
    }

    chat(
        State(state),
        Extension(cancel_token),
        headers,
        Json(request),
        &request_id,
    )
    .await
}

pub(crate) async fn chat(
    State(state): State<Arc<AppState>>,
    Extension(cancel_token): Extension<CancellationToken>,
    headers: HeaderMap,
    Json(mut request): Json<ChatCompletionRequest>,
    request_id: impl AsRef<str>,
) -> ServerResult<axum::response::Response> {
    let request_id = request_id.as_ref();

    // Get target server
    let chat_server = get_chat_server(&state, request_id).await?;

    // Send request and handle response
    let response = send_request_with_retry(
        &chat_server,
        &mut request,
        &headers,
        request_id,
        cancel_token.clone(),
    )
    .await?;

    // Handle response based on stream mode
    match request.stream {
        Some(true) => {
            // Handle stream response
            handle_stream_response(
                response,
                &mut request,
                &headers,
                &chat_server,
                request_id,
                cancel_token,
            )
            .await
        }
        Some(false) | None => {
            // Handle non-stream response
            handle_non_stream_response(
                response,
                &mut request,
                &headers,
                &chat_server,
                request_id,
                cancel_token,
            )
            .await
        }
    }
}

pub(crate) async fn embeddings_handler(
    State(state): State<Arc<AppState>>,
    Extension(cancel_token): Extension<CancellationToken>,
    headers: HeaderMap,
    Json(request): Json<EmbeddingRequest>,
) -> ServerResult<axum::response::Response> {
    // Get request ID from headers
    let request_id = headers
        .get("x-request-id")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("unknown")
        .to_string();

    dual_info!(
        "Received a new embeddings request - request_id: {}",
        request_id
    );

    // get the embeddings server
    let servers = state.server_group.read().await;
    let embeddings_servers = match servers.get(&ServerKind::embeddings) {
        Some(servers) => servers,
        None => {
            let err_msg = "No embeddings server available";
            dual_error!("{} - request_id: {}", err_msg, request_id);
            return Err(ServerError::Operation(err_msg.to_string()));
        }
    };

    let embedding_server = match embeddings_servers.next().await {
        Ok(target_server_info) => target_server_info,
        Err(e) => {
            let err_msg = format!("Failed to get the embeddings server: {e}");
            dual_error!("{} - request_id: {}", err_msg, request_id);
            return Err(ServerError::Operation(err_msg));
        }
    };
    let embeddings_service_url =
        format!("{}/embeddings", embedding_server.url.trim_end_matches('/'));
    dual_info!(
        "Forward the embeddings request to {} - request_id: {}",
        embeddings_service_url,
        request_id
    );

    // parse the content-type header
    let content_type = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| {
            let err_msg = "Missing Content-Type header".to_string();
            dual_error!("{} - request_id: {}", err_msg, request_id);
            ServerError::Operation(err_msg)
        })?;
    let content_type = content_type.to_string();
    dual_debug!(
        "Request content type: {} - request_id: {}",
        content_type,
        request_id
    );

    // Create request client
    let ds_request = if let Some(api_key) = &embedding_server.api_key
        && !api_key.is_empty()
    {
        reqwest::Client::new()
            .post(embeddings_service_url)
            .header("Content-Type", content_type)
            .header(AUTHORIZATION, api_key)
            .json(&request)
    } else if headers.contains_key("authorization") {
        let authorization = headers
            .get("authorization")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();

        reqwest::Client::new()
            .post(embeddings_service_url)
            .header("Content-Type", content_type)
            .header("Authorization", authorization)
            .json(&request)
    } else {
        reqwest::Client::new()
            .post(embeddings_service_url)
            .header("Content-Type", content_type)
            .json(&request)
    };

    // Use select! to handle request cancellation
    let ds_response = select! {
        response = ds_request.send() => {
            response.map_err(|e| {
                let err_msg = format!(
                    "Failed to forward the request to the downstream server: {e}",
                );
                dual_error!("{err_msg} - request_id: {request_id}");
                ServerError::Operation(err_msg)
            })?
        }
        _ = cancel_token.cancelled() => {
            let warn_msg = "Request was cancelled by client";
            dual_warn!("{} - request_id: {}", warn_msg, request_id);
            return Err(ServerError::Operation(warn_msg.to_string()));
        }
    };

    let status = ds_response.status();

    // Handle response body reading with cancellation
    let bytes = select! {
        bytes = ds_response.bytes() => {
            bytes.map_err(|e| {
                let err_msg = format!("Failed to get the full response as bytes: {e}");
                dual_error!("{err_msg} - request_id: {request_id}");
                ServerError::Operation(err_msg)
            })?
        }
        _ = cancel_token.cancelled() => {
            let warn_msg = "Request was cancelled while reading response";
            dual_warn!("{} - request_id: {}", warn_msg, request_id);
            return Err(ServerError::Operation(warn_msg.to_string()));
        }
    };

    match Response::builder()
        .status(status)
        .header("Content-Type", "application/json")
        .body(Body::from(bytes))
    {
        Ok(response) => {
            dual_info!(
                "Embeddings request completed successfully - request_id: {}",
                request_id
            );
            Ok(response)
        }
        Err(e) => {
            let err_msg = format!("Failed to create the response: {e}");
            dual_error!("{err_msg} - request_id: {request_id}");
            Err(ServerError::Operation(err_msg))
        }
    }
}

pub(crate) async fn audio_transcriptions_handler(
    State(state): State<Arc<AppState>>,
    Extension(cancel_token): Extension<CancellationToken>,
    req: axum::extract::Request<Body>,
) -> ServerResult<axum::response::Response> {
    // Get request ID from headers
    let request_id = req
        .headers()
        .get("x-request-id")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("unknown")
        .to_string();

    dual_info!(
        "Received a new audio transcription request - request_id: {}",
        request_id
    );

    // get the transcribe server
    let transcription_server = {
        let servers = state.server_group.read().await;
        let transcribe_servers = match servers.get(&ServerKind::transcribe) {
            Some(servers) => servers,
            None => {
                let err_msg = "No transcribe server available";
                dual_error!("{} - request_id: {}", err_msg, request_id);
                return Err(ServerError::Operation(err_msg.to_string()));
            }
        };

        match transcribe_servers.next().await {
            Ok(target_server_info) => target_server_info,
            Err(e) => {
                let err_msg = format!("Failed to get the transcribe server: {e}");
                dual_error!("{} - request_id: {}", err_msg, request_id);
                return Err(ServerError::Operation(err_msg));
            }
        }
    };

    let transcription_server_url = format!(
        "{}/audio/transcriptions",
        transcription_server.url.trim_end_matches('/')
    );
    dual_info!(
        "Forward the audio transcription request to {} - request_id: {}",
        transcription_server_url,
        request_id
    );

    // Create request client
    let mut ds_request = reqwest::Client::new().post(transcription_server_url);
    if let Some(api_key) = &transcription_server.api_key
        && !api_key.is_empty()
    {
        ds_request = ds_request.header(AUTHORIZATION, api_key);
    }
    for (name, value) in req.headers().iter() {
        ds_request = ds_request.header(name, value);
    }

    // convert the request body into bytes
    let body = req.into_body();
    let body_bytes = axum::body::to_bytes(body, usize::MAX).await.map_err(|e| {
        let err_msg = format!("Failed to convert the request body into bytes: {e}");
        dual_error!("{err_msg} - request_id: {request_id}");
        ServerError::Operation(err_msg)
    })?;

    ds_request = ds_request.body(body_bytes);

    // Use select! to handle request cancellation
    let ds_response = select! {
        response = ds_request.send() => {
            response.map_err(|e| {
                let err_msg = format!(
                    "Failed to forward the request to the downstream server: {e}"
                );
                dual_error!("{err_msg} - request_id: {request_id}");
                ServerError::Operation(err_msg)
            })?
        }
        _ = cancel_token.cancelled() => {
            let warn_msg = "Request was cancelled by client";
            dual_warn!("{} - request_id: {}", warn_msg, request_id);
            return Err(ServerError::Operation(warn_msg.to_string()));
        }
    };

    let status = ds_response.status();

    // Handle response body reading with cancellation
    let bytes = select! {
        bytes = ds_response.bytes() => {
            bytes.map_err(|e| {
                let err_msg = format!("Failed to get the full response as bytes: {e}");
                dual_error!("{err_msg} - request_id: {request_id}");
                ServerError::Operation(err_msg)
            })?
        }
        _ = cancel_token.cancelled() => {
            let warn_msg = "Request was cancelled while reading response";
            dual_warn!("{} - request_id: {}", warn_msg, request_id);
            return Err(ServerError::Operation(warn_msg.to_string()));
        }
    };

    match Response::builder()
        .status(status)
        .header("Content-Type", "application/json")
        .body(Body::from(bytes))
    {
        Ok(response) => {
            dual_info!(
                "Audio transcription request completed successfully - request_id: {}",
                request_id
            );
            Ok(response)
        }
        Err(e) => {
            let err_msg = format!("Failed to create the response: {e}");
            dual_error!("{err_msg} - request_id: {request_id}");
            Err(ServerError::Operation(err_msg))
        }
    }
}

pub(crate) async fn audio_translations_handler(
    State(state): State<Arc<AppState>>,
    Extension(cancel_token): Extension<CancellationToken>,
    req: axum::extract::Request<Body>,
) -> ServerResult<axum::response::Response> {
    // Get request ID from headers
    let request_id = req
        .headers()
        .get("x-request-id")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("unknown")
        .to_string();

    dual_info!(
        "Received a new audio translation request - request_id: {}",
        request_id
    );

    // get the transcribe server
    let translation_server = {
        let servers = state.server_group.read().await;
        let translate_servers = match servers.get(&ServerKind::translate) {
            Some(servers) => servers,
            None => {
                let err_msg = "No translate server available";
                dual_error!("{} - request_id: {}", err_msg, request_id);
                return Err(ServerError::Operation(err_msg.to_string()));
            }
        };

        match translate_servers.next().await {
            Ok(target_server_info) => target_server_info,
            Err(e) => {
                let err_msg = format!("Failed to get the translate server: {e}");
                dual_error!("{} - request_id: {}", err_msg, request_id);
                return Err(ServerError::Operation(err_msg));
            }
        }
    };

    let translation_server_url = format!(
        "{}/audio/translations",
        translation_server.url.trim_end_matches('/')
    );
    dual_info!(
        "Forward the audio translation request to {} - request_id: {}",
        translation_server_url,
        request_id
    );

    // Create request client
    let mut ds_request = reqwest::Client::new().post(translation_server_url);
    if let Some(api_key) = &translation_server.api_key
        && !api_key.is_empty()
    {
        ds_request = ds_request.header(AUTHORIZATION, api_key);
    }
    for (name, value) in req.headers().iter() {
        ds_request = ds_request.header(name, value);
    }

    // convert the request body into bytes
    let body = req.into_body();
    let body_bytes = axum::body::to_bytes(body, usize::MAX).await.map_err(|e| {
        let err_msg = format!("Failed to convert the request body into bytes: {e}");
        dual_error!("{err_msg} - request_id: {request_id}");
        ServerError::Operation(err_msg)
    })?;

    ds_request = ds_request.body(body_bytes);

    // Use select! to handle request cancellation
    let ds_response = select! {
        response = ds_request.send() => {
            response.map_err(|e| {
                let err_msg = format!(
                    "Failed to forward the request to the downstream server: {e}"
                );
                dual_error!("{err_msg} - request_id: {request_id}");
                ServerError::Operation(err_msg)
            })?
        }
        _ = cancel_token.cancelled() => {
            let warn_msg = "Request was cancelled by client";
            dual_warn!("{} - request_id: {}", warn_msg, request_id);
            return Err(ServerError::Operation(warn_msg.to_string()));
        }
    };

    let status = ds_response.status();

    // Handle response body reading with cancellation
    let bytes = select! {
        bytes = ds_response.bytes() => {
            bytes.map_err(|e| {
                let err_msg = format!("Failed to get the full response as bytes: {e}");
                dual_error!("{err_msg} - request_id: {request_id}");
                ServerError::Operation(err_msg)
            })?
        }
        _ = cancel_token.cancelled() => {
            let warn_msg = "Request was cancelled while reading response";
            dual_warn!("{} - request_id: {}", warn_msg, request_id);
            return Err(ServerError::Operation(warn_msg.to_string()));
        }
    };

    match Response::builder()
        .status(status)
        .header("Content-Type", "application/json")
        .body(Body::from(bytes))
    {
        Ok(response) => {
            dual_info!(
                "Audio translation request completed successfully - request_id: {}",
                request_id
            );
            Ok(response)
        }
        Err(e) => {
            let err_msg = format!("Failed to create the response: {e}");
            dual_error!("{err_msg} - request_id: {request_id}");
            Err(ServerError::Operation(err_msg))
        }
    }
}

pub(crate) async fn audio_tts_handler(
    State(state): State<Arc<AppState>>,
    Extension(cancel_token): Extension<CancellationToken>,
    req: axum::extract::Request<Body>,
) -> ServerResult<axum::response::Response> {
    // Get request ID from headers
    let request_id = req
        .headers()
        .get("x-request-id")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("unknown")
        .to_string();

    dual_info!(
        "Received a new audio speech request - request_id: {}",
        request_id
    );

    // get the tts server
    let tts_server = {
        let servers = state.server_group.read().await;
        let tts_servers = match servers.get(&ServerKind::tts) {
            Some(servers) => servers,
            None => {
                let err_msg = "No tts server available";
                dual_error!("{} - request_id: {}", err_msg, request_id);
                return Err(ServerError::Operation(err_msg.to_string()));
            }
        };

        match tts_servers.next().await {
            Ok(target_server_info) => target_server_info,
            Err(e) => {
                let err_msg = format!("Failed to get the tts server: {e}");
                dual_error!("{} - request_id: {}", err_msg, request_id);
                return Err(ServerError::Operation(err_msg));
            }
        }
    };

    let tts_server_url = format!("{}/audio/speech", tts_server.url.trim_end_matches('/'));
    dual_info!(
        "Forward the audio speech request to {} - request_id: {}",
        tts_server_url,
        request_id
    );

    // Create request client
    let mut ds_request = reqwest::Client::new().post(tts_server_url);
    if let Some(api_key) = &tts_server.api_key
        && !api_key.is_empty()
    {
        ds_request = ds_request.header(AUTHORIZATION, api_key);
    }
    for (name, value) in req.headers().iter() {
        ds_request = ds_request.header(name, value);
    }

    let body = req.into_body();
    let body_bytes = axum::body::to_bytes(body, usize::MAX).await.map_err(|e| {
        let err_msg = format!("Failed to convert the request body into bytes: {e}");
        dual_error!("{err_msg} - request_id: {request_id}");
        ServerError::Operation(err_msg)
    })?;

    ds_request = ds_request.body(body_bytes);

    // Use select! to handle request cancellation
    let ds_response = select! {
        response = ds_request.send() => {
            response.map_err(|e| {
                let err_msg = format!(
                    "Failed to forward the request to the downstream server: {e}"
                );
                dual_error!("{err_msg} - request_id: {request_id}");
                ServerError::Operation(err_msg)
            })?
        }
        _ = cancel_token.cancelled() => {
            let warn_msg = "Request was cancelled by client";
            dual_warn!("{} - request_id: {}", warn_msg, request_id);
            return Err(ServerError::Operation(warn_msg.to_string()));
        }
    };

    // create a response builder with the status and headers of the downstream response
    let mut response_builder = Response::builder().status(ds_response.status());
    for (name, value) in ds_response.headers().iter() {
        response_builder = response_builder.header(name, value);
    }

    // Handle response body reading with cancellation
    let bytes = select! {
        bytes = ds_response.bytes() => {
            bytes.map_err(|e| {
                let err_msg = format!("Failed to get the full response as bytes: {e}");
                dual_error!("{err_msg} - request_id: {request_id}");
                ServerError::Operation(err_msg)
            })?
        }
        _ = cancel_token.cancelled() => {
            let warn_msg = "Request was cancelled while reading response";
            dual_warn!("{} - request_id: {}", warn_msg, request_id);
            return Err(ServerError::Operation(warn_msg.to_string()));
        }
    };

    match response_builder.body(Body::from(bytes)) {
        Ok(response) => {
            dual_info!(
                "Audio speech request completed successfully - request_id: {}",
                request_id
            );
            Ok(response)
        }
        Err(e) => {
            let err_msg = format!("Failed to create the response: {e}");
            dual_error!("{err_msg} - request_id: {request_id}");
            Err(ServerError::Operation(err_msg))
        }
    }
}

pub(crate) async fn image_handler(
    State(state): State<Arc<AppState>>,
    Extension(cancel_token): Extension<CancellationToken>,
    req: axum::extract::Request<Body>,
) -> ServerResult<axum::response::Response> {
    // Get request ID from headers
    let request_id = req
        .headers()
        .get("x-request-id")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("unknown")
        .to_string();

    dual_info!("Received a new image request - request_id: {}", request_id);

    // get the image server
    let image_server = {
        let servers = state.server_group.read().await;
        let image_servers = match servers.get(&ServerKind::image) {
            Some(servers) => servers,
            None => {
                let err_msg = "No image server available";
                dual_error!("{} - request_id: {}", err_msg, request_id);
                return Err(ServerError::Operation(err_msg.to_string()));
            }
        };

        match image_servers.next().await {
            Ok(target_server_info) => target_server_info,
            Err(e) => {
                let err_msg = format!("Failed to get the image server: {e}");
                dual_error!("{} - request_id: {}", err_msg, request_id);
                return Err(ServerError::Operation(err_msg));
            }
        }
    };

    let image_server_url = format!(
        "{}/images/generations",
        image_server.url.trim_end_matches('/')
    );
    dual_info!(
        "Forward the image request to {} - request_id: {}",
        image_server_url,
        request_id
    );

    // Create request client
    let mut ds_request = reqwest::Client::new().post(image_server_url);
    if let Some(api_key) = &image_server.api_key
        && !api_key.is_empty()
    {
        ds_request = ds_request.header(AUTHORIZATION, api_key);
    }
    for (name, value) in req.headers().iter() {
        ds_request = ds_request.header(name, value);
    }

    // convert the request body into bytes
    let body = req.into_body();
    let body_bytes = axum::body::to_bytes(body, usize::MAX).await.map_err(|e| {
        let err_msg = format!("Failed to convert the request body into bytes: {e}");
        dual_error!("{err_msg} - request_id: {request_id}");
        ServerError::Operation(err_msg)
    })?;

    ds_request = ds_request.body(body_bytes);

    // Use select! to handle request cancellation
    let ds_response = select! {
        response = ds_request.send() => {
            response.map_err(|e| {
                let err_msg = format!(
                    "Failed to forward the request to the downstream server: {e}"
                );
                dual_error!("{err_msg} - request_id: {request_id}");
                ServerError::Operation(err_msg)
            })?
        }
        _ = cancel_token.cancelled() => {
            let warn_msg = "Request was cancelled by client";
            dual_warn!("{} - request_id: {}", warn_msg, request_id);
            return Err(ServerError::Operation(warn_msg.to_string()));
        }
    };

    // create a response builder with the status and headers of the downstream response
    let mut response_builder = Response::builder().status(ds_response.status());
    for (name, value) in ds_response.headers().iter() {
        response_builder = response_builder.header(name, value);
    }

    // Handle response body reading with cancellation
    let bytes = select! {
        bytes = ds_response.bytes() => {
            bytes.map_err(|e| {
                let err_msg = format!("Failed to get the full response as bytes: {e}");
                dual_error!("{err_msg} - request_id: {request_id}");
                ServerError::Operation(err_msg)
            })?
        }
        _ = cancel_token.cancelled() => {
            let warn_msg = "Request was cancelled while reading response";
            dual_warn!("{} - request_id: {}", warn_msg, request_id);
            return Err(ServerError::Operation(warn_msg.to_string()));
        }
    };

    match response_builder.body(Body::from(bytes)) {
        Ok(response) => {
            dual_info!(
                "Image request completed successfully - request_id: {}",
                request_id
            );
            Ok(response)
        }
        Err(e) => {
            let err_msg = format!("Failed to create the response: {e}");
            dual_error!("{err_msg} - request_id: {request_id}");
            Err(ServerError::Operation(err_msg))
        }
    }
}

pub(crate) async fn models_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> ServerResult<axum::response::Response> {
    let request_id = headers
        .get("x-request-id")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("unknown")
        .to_string();

    let models = state.models.read().await;
    let list_response = ListModelsResponse {
        object: String::from("list"),
        data: models.values().flatten().cloned().collect(),
    };

    let json_body = serde_json::to_string(&list_response).map_err(|e| {
        let err_msg = format!("Failed to serialize the models: {e}");
        dual_error!("{err_msg} - request_id: {request_id}");
        ServerError::Operation(err_msg)
    })?;

    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .body(Body::from(json_body))
        .map_err(|e| {
            let err_msg = format!("Failed to create response: {e}");
            dual_error!("{err_msg} - request_id: {request_id}");
            ServerError::Operation(err_msg)
        })
}

pub(crate) async fn info_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> ServerResult<axum::response::Response> {
    let request_id = headers
        .get("x-request-id")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("unknown")
        .to_string();

    let mut chat_models = vec![];
    let mut embedding_models = vec![];
    let mut image_models = vec![];
    let mut tts_models = vec![];
    let mut translate_models = vec![];
    let mut transcribe_models = vec![];
    let server_info = state.server_info.read().await;
    for server in server_info.servers.values() {
        if let Some(ref model) = server.chat_model {
            chat_models.push(model.clone());
        }
        if let Some(ref model) = server.embedding_model {
            embedding_models.push(model.clone());
        }
        if let Some(ref model) = server.image_model {
            image_models.push(model.clone());
        }
        if let Some(ref model) = server.tts_model {
            tts_models.push(model.clone());
        }
        if let Some(ref model) = server.translate_model {
            translate_models.push(model.clone());
        }
        if let Some(ref model) = server.transcribe_model {
            transcribe_models.push(model.clone());
        }
    }

    let json_body = serde_json::json!({
        "models": {
            "chat": chat_models,
            "embedding": embedding_models,
            "image": image_models,
            "tts": tts_models,
            "translate": translate_models,
            "transcribe": transcribe_models,
        },
    });

    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .body(Body::from(json_body.to_string()))
        .map_err(|e| {
            let err_msg = format!("Failed to create response: {e}");
            dual_error!("{err_msg} - request_id: {request_id}");
            ServerError::Operation(err_msg)
        })
}

pub(crate) mod admin {
    use super::*;

    pub(crate) async fn register_downstream_server_handler(
        State(state): State<Arc<AppState>>,
        headers: HeaderMap,
        Json(mut server): Json<Server>,
    ) -> ServerResult<axum::response::Response> {
        // Get request ID from headers
        let request_id = headers
            .get("x-request-id")
            .and_then(|h| h.to_str().ok())
            .unwrap_or("unknown")
            .to_string();

        let server_url = server.url.clone();
        let server_kind = server.kind;
        let server_id = server.id.clone();

        // verify the server
        if server_kind.contains(ServerKind::chat)
            || server_kind.contains(ServerKind::embeddings)
            || server_kind.contains(ServerKind::image)
            || server_kind.contains(ServerKind::transcribe)
            || server_kind.contains(ServerKind::translate)
            || server_kind.contains(ServerKind::tts)
        {
            dual_warn!(
                "Ignore the server verification for: {server_id} - request_id: {request_id}"
            );
            // _verify_server(State(state.clone()), &headers, &request_id, &server).await?;
        }

        // update the model list
        update_model_list(State(state.clone()), &headers, &request_id, &server).await?;

        // update health status of the server
        server.health_status.is_healthy = true;
        server.health_status.last_check = SystemTime::now();

        // register the server
        state.register_downstream_server(server).await?;
        dual_info!(
            "Registered successfully. Assigned Server Id: {} - request_id: {}",
            server_id,
            request_id
        );

        // create a response with status code 200. Content-Type is JSON
        let json_body = serde_json::json!({
            "id": server_id,
            "url": server_url,
            "kind": server_kind
        });

        let response = Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "application/json")
            .body(Body::from(json_body.to_string()))
            .map_err(|e| {
                let err_msg = format!("Failed to create response: {e}");
                dual_error!("{err_msg} - request_id: {request_id}");
                ServerError::Operation(err_msg)
            })?;

        Ok(response)
    }

    // verify the server and get the server info and model list
    async fn _verify_server(
        State(state): State<Arc<AppState>>,
        headers: &HeaderMap,
        request_id: impl AsRef<str>,
        server: &Server,
    ) -> ServerResult<()> {
        let request_id = request_id.as_ref();
        let server_url = &server.url;
        let server_id = &server.id;
        let server_kind = server.kind;

        let server_info_url = format!("{server_url}/info");

        let client = reqwest::Client::new();
        let response = if let Some(api_key) = &server.api_key
            && !api_key.is_empty()
        {
            client
                .get(&server_info_url)
                .header(CONTENT_TYPE, "application/json")
                .header(AUTHORIZATION, api_key)
                .send()
                .await
                .map_err(|e| {
                    let err_msg =
                        format!("Failed to verify the {server_kind} downstream server: {e}",);
                    dual_error!("{err_msg} - request_id: {request_id}");
                    ServerError::Operation(err_msg)
                })?
        } else if headers.contains_key("authorization") {
            let authorization = headers
                .get("authorization")
                .unwrap()
                .to_str()
                .unwrap()
                .to_string();

            client
                .get(&server_info_url)
                .header(CONTENT_TYPE, "application/json")
                .header(AUTHORIZATION, authorization)
                .send()
                .await
                .map_err(|e| {
                    let err_msg =
                        format!("Failed to verify the {server_kind} downstream server: {e}",);
                    dual_error!("{err_msg} - request_id: {request_id}");
                    ServerError::Operation(err_msg)
                })?
        } else {
            client.get(&server_info_url).send().await.map_err(|e| {
                let err_msg = format!("Failed to verify the {server_kind} downstream server: {e}",);
                dual_error!("{err_msg} - request_id: {request_id}");
                ServerError::Operation(err_msg)
            })?
        };
        if !response.status().is_success() {
            let err_msg = format!(
                "Failed to verify the {} downstream server: {}",
                server_kind,
                response.status()
            );
            dual_error!("{} - request_id: {}", err_msg, request_id);
            return Err(ServerError::Operation(err_msg));
        }

        let mut api_server = response.json::<ApiServer>().await.map_err(|e| {
            let err_msg = format!("Failed to parse the server info: {e}");
            dual_error!("{err_msg} - request_id: {request_id}");
            ServerError::Operation(err_msg)
        })?;
        api_server.server_id = Some(server_id.to_string());

        dual_debug!("server kind: {}", server_kind.to_string());
        dual_debug!("api server: {:?}", api_server);

        // verify the server kind
        {
            if server_kind.contains(ServerKind::chat) && api_server.chat_model.is_none() {
                let err_msg = "You are trying to register a chat server. However, the server does not support `chat`. Please check the server kind.";
                dual_error!("{} - request_id: {}", err_msg, request_id);
                return Err(ServerError::Operation(err_msg.to_string()));
            }
            if server_kind.contains(ServerKind::embeddings) && api_server.embedding_model.is_none()
            {
                let err_msg = "You are trying to register an embedding server. However, the server does not support `embeddings`. Please check the server kind.";
                dual_error!("{} - request_id: {}", err_msg, request_id);
                return Err(ServerError::Operation(err_msg.to_string()));
            }
            if server_kind.contains(ServerKind::image) && api_server.image_model.is_none() {
                let err_msg = "You are trying to register an image server. However, the server does not support `image`. Please check the server kind.";
                dual_error!("{} - request_id: {}", err_msg, request_id);
                return Err(ServerError::Operation(err_msg.to_string()));
            }
            if server_kind.contains(ServerKind::tts) && api_server.tts_model.is_none() {
                let err_msg = "You are trying to register a TTS server. However, the server does not support `tts`. Please check the server kind.";
                dual_error!("{} - request_id: {}", err_msg, request_id);
                return Err(ServerError::Operation(err_msg.to_string()));
            }
            if server_kind.contains(ServerKind::translate) && api_server.translate_model.is_none() {
                let err_msg = "You are trying to register a translation server. However, the server does not support `translate`. Please check the server kind.";
                dual_error!("{} - request_id: {}", err_msg, request_id);
                return Err(ServerError::Operation(err_msg.to_string()));
            }
            if server_kind.contains(ServerKind::transcribe) && api_server.transcribe_model.is_none()
            {
                let err_msg = "You are trying to register a transcription server. However, the server does not support `transcribe`. Please check the server kind.";
                dual_error!("{} - request_id: {}", err_msg, request_id);
                return Err(ServerError::Operation(err_msg.to_string()));
            }
        }

        // update the server info
        let server_info = &mut state.server_info.write().await;
        server_info
            .servers
            .insert(server_id.to_string(), api_server);

        Ok(())
    }

    // update the model list
    pub(crate) async fn update_model_list(
        State(state): State<Arc<AppState>>,
        headers: &HeaderMap,
        request_id: impl AsRef<str>,
        server: &Server,
    ) -> ServerResult<()> {
        let request_id = request_id.as_ref();
        let server_url = &server.url;
        let server_id = &server.id;

        // get the models from the downstream server
        let list_models_url = format!("{server_url}/models");
        dual_debug!("list_models_url: {}", list_models_url);
        let response = if let Some(api_key) = &server.api_key
            && !api_key.is_empty()
        {
            reqwest::Client::new()
                .get(&list_models_url)
                .header(CONTENT_TYPE, "application/json")
                .header(AUTHORIZATION, api_key)
                .send()
                .await
                .map_err(|e| {
                    let err_msg =
                        format!("Failed to get the models from the downstream server: {e}");
                    dual_error!("{err_msg} - request_id: {request_id}");
                    ServerError::Operation(err_msg)
                })?
        } else if headers.contains_key("authorization") {
            let authorization = headers
                .get("authorization")
                .unwrap()
                .to_str()
                .unwrap()
                .to_string();
            reqwest::Client::new()
                .get(&list_models_url)
                .header(CONTENT_TYPE, "application/json")
                .header(AUTHORIZATION, authorization)
                .send()
                .await
                .map_err(|e| {
                    let err_msg =
                        format!("Failed to get the models from the downstream server: {e}");
                    dual_error!("{err_msg} - request_id: {request_id}");
                    ServerError::Operation(err_msg)
                })?
        } else {
            reqwest::Client::new()
                .get(&list_models_url)
                .send()
                .await
                .map_err(|e| {
                    let err_msg =
                        format!("Failed to get the models from the downstream server: {e}");
                    dual_error!("{err_msg} - request_id: {request_id}");
                    ServerError::Operation(err_msg)
                })?
        };
        let status = response.status();
        if !status.is_success() {
            let err_msg =
                format!("Status: {status}. Failed to get model info from {list_models_url}.",);
            dual_error!("{} - request_id: {}", err_msg, request_id);
            return Err(ServerError::Operation(err_msg));
        }

        match server_url.as_str() {
            "https://openrouter.ai/api/v1" => {
                let list_models_response =
                    response.json::<serde_json::Value>().await.map_err(|e| {
                        let err_msg =
                            format!("Failed to get the models from {list_models_url}: {e}");
                        dual_error!("{err_msg} - request_id: {request_id}");
                        ServerError::Operation(err_msg)
                    })?;

                match list_models_response.get("data") {
                    Some(data) => {
                        // get `id` field from each model
                        let models = data.as_array().unwrap();
                        let model_info_vec = models
                            .iter()
                            .map(|model| {
                                let id = model.get("id").unwrap().as_str().unwrap();
                                let created = model.get("created").unwrap().as_u64().unwrap();
                                Model {
                                    id: id.to_string(),
                                    created,
                                    object: "model".to_string(),
                                    owned_by: "openrouter.ai".to_string(),
                                }
                            })
                            .collect::<Vec<Model>>();

                        // update the models
                        let mut models = state.models.write().await;
                        models.insert(server_id.to_string(), model_info_vec);
                    }
                    None => {
                        let err_msg = format!(
                            "Failed to get the models from {list_models_url}. Not found `data` field in the response."
                        );
                        dual_error!("{err_msg} - request_id: {request_id}");
                        return Err(ServerError::Operation(err_msg.to_string()));
                    }
                }
            }
            _ => {
                let list_models_response =
                    response.json::<ListModelsResponse>().await.map_err(|e| {
                        let err_msg =
                            format!("Failed to get the models from {list_models_url}: {e}");
                        dual_error!("{err_msg} - request_id: {request_id}");
                        ServerError::Operation(err_msg)
                    })?;

                // update the models
                let mut models = state.models.write().await;
                models.insert(server_id.to_string(), list_models_response.data);
            }
        }

        Ok(())
    }

    pub(crate) async fn remove_downstream_server_handler(
        State(state): State<Arc<AppState>>,
        headers: HeaderMap,
        Json(server_id): Json<ServerIdToRemove>,
    ) -> ServerResult<axum::response::Response> {
        // Get request ID from headers
        let request_id = headers
            .get("x-request-id")
            .and_then(|h| h.to_str().ok())
            .unwrap_or("unknown")
            .to_string();

        state
            .unregister_downstream_server(&server_id.server_id)
            .await?;

        // create a response with status code 200. Content-Type is JSON
        let json_body = serde_json::json!({
            "message": "Server unregistered successfully.",
            "id": server_id.server_id,
        });

        let response = Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "application/json")
            .body(Body::from(json_body.to_string()))
            .map_err(|e| {
                let err_msg = format!("Failed to create response: {e}");
                dual_error!("{err_msg} - request_id: {request_id}");
                ServerError::Operation(err_msg)
            })?;

        Ok(response)
    }

    pub(crate) async fn list_downstream_servers_handler(
        State(state): State<Arc<AppState>>,
        headers: HeaderMap,
    ) -> ServerResult<axum::response::Response> {
        // Get request ID from headers
        let request_id = headers
            .get("x-request-id")
            .and_then(|h| h.to_str().ok())
            .unwrap_or("unknown")
            .to_string();

        let servers = state.list_downstream_servers().await?;

        // compute the total number of servers
        let total_servers = servers.values().fold(0, |acc, servers| acc + servers.len());
        dual_info!(
            "Found {} downstream servers - request_id: {}",
            total_servers,
            request_id
        );

        let json_body = serde_json::to_string(&servers).unwrap();

        let response = Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "application/json")
            .body(Body::from(json_body))
            .map_err(|e| {
                let err_msg = format!("Failed to create response: {e}");
                dual_error!("{err_msg} - request_id: {request_id}");
                ServerError::Operation(err_msg)
            })?;

        Ok(response)
    }
}

// Generate a unique chat id for the chat completion request
fn gen_chat_id() -> String {
    format!("chatcmpl-{}", uuid::Uuid::new_v4())
}

async fn get_chat_server(
    state: &Arc<AppState>,
    request_id: &str,
) -> ServerResult<crate::server::TargetServerInfo> {
    let servers = state.server_group.read().await;
    let chat_servers = match servers.get(&ServerKind::chat) {
        Some(servers) => servers,
        None => {
            let err_msg = "No chat server available";
            dual_error!("{} - request_id: {}", err_msg, request_id);
            return Err(ServerError::Operation(err_msg.to_string()));
        }
    };

    match chat_servers.next().await {
        Ok(target_server_info) => Ok(target_server_info),
        Err(e) => {
            let err_msg = format!("Failed to get the chat server: {e}");
            dual_error!("{} - request_id: {}", err_msg, request_id);
            Err(ServerError::Operation(err_msg))
        }
    }
}

/// Send chat request to downstream server with intelligent retry mechanism
///
/// This function implements the following features:
/// 1. First attempt to send request to downstream server
/// 2. If tool call deserialization error occurs, intelligently retry:
///    - Check if request contains tool definitions
///    - Check if current tool choice is non-None state
///    - If conditions are met, reset tool choice to None and retry
/// 3. This retry mechanism solves cases where some downstream servers don't support tool calls
///
/// # Arguments
///
/// * `chat_server` - The downstream chat server to send request to
/// * `request` - Chat completion request, may be modified (e.g., reset tool choice)
/// * `headers` - HTTP request headers, including authentication info
/// * `request_id` - Request ID for log tracking
/// * `cancel_token` - Cancellation token for request cancellation support
///
/// # Returns
/// * `Ok(response)` - Successfully obtained downstream server response
/// * `Err(ServerError)` - Request failed or still failed after retry
///
/// # Error Handling Strategy
/// * Tool call deserialization error: Try disabling tool choice and retry
/// * Other errors: Return error directly, no retry
/// * Retry logic: Maximum one retry to avoid infinite loops
async fn send_request_with_retry(
    chat_server: &TargetServerInfo,
    request: &mut ChatCompletionRequest,
    headers: &HeaderMap,
    request_id: &str,
    cancel_token: CancellationToken,
) -> ServerResult<reqwest::Response> {
    // First attempt to send request to downstream server
    let response = build_and_send_request(
        chat_server,
        request,
        headers,
        cancel_token.clone(),
        request_id,
    )
    .await;

    match response {
        // If first request succeeds, return response directly
        Ok(response) => Ok(response),
        Err(e) => {
            let err_str = e.to_string();

            // Check if it's a tool call deserialization error
            // This error usually occurs when downstream server doesn't support tool calls
            if err_str.contains("Failed to deserialize generated tool calls") {
                // Verify if retry is possible:
                // 1. Request must contain tool definitions
                // 2. Tool definitions cannot be empty
                if let Some(tools) = &request.tools
                    && !tools.is_empty()
                {
                    // Check if current tool choice is non-None state
                    // Only non-None state needs to be reset to None for retry
                    if let Some(tool_choice) = &request.tool_choice
                        && *tool_choice != ToolChoice::None
                    {
                        // Reset tool choice to None, disable tool call functionality
                        request.tool_choice = None;
                        dual_info!(
                            "Retrying request without tool choice - request_id: {}",
                            request_id
                        );

                        // Re-send with reset request
                        let response = build_and_send_request(
                            chat_server,
                            request,
                            headers,
                            cancel_token,
                            request_id,
                        )
                        .await
                        .map_err(|e| {
                            let err_msg = format!("Failed to send request: {e}");
                            dual_error!("{} - request_id: {}", err_msg, request_id);
                            ServerError::Operation(err_msg)
                        })?;

                        return Ok(response);
                    }
                }
            }

            // Non-tool call related error, return directly, no retry
            let err_msg = format!("Failed to send request: {e}");
            dual_error!("{} - request_id: {}", err_msg, request_id);
            Err(ServerError::Operation(err_msg))
        }
    }
}

/// Build and send HTTP request to downstream server with cancellation support
///
/// This function implements the following features:
/// 1. Build HTTP client and set necessary request headers
/// 2. Send JSON-formatted chat completion request to downstream server
/// 3. Support cancellation of ongoing requests via CancellationToken
/// 4. Provide detailed error information and cancellation logs
///
/// # Arguments
///
/// * `chat_server` - The downstream chat server to send request to
/// * `request` - Chat completion request object
/// * `headers` - HTTP request headers, including authentication info
/// * `cancel_token` - Cancellation token for request cancellation support
///
/// # Returns
/// * `Ok(response)` - Successfully obtained downstream server response
/// * `Err(ServerError)` - Request failed or was cancelled
///
/// # Cancellation Features
/// * When cancel_token is triggered, function immediately returns cancellation error
/// * Cancellation logs warning messages for debugging and monitoring
/// * Cancellation operation releases related resources to prevent leaks
async fn build_and_send_request(
    chat_server: &TargetServerInfo,
    request: &ChatCompletionRequest,
    headers: &HeaderMap,
    cancel_token: CancellationToken,
    request_id: &str,
) -> ServerResult<reqwest::Response> {
    let url = format!("{}/chat/completions", chat_server.url.trim_end_matches('/'));
    let mut client = reqwest::Client::new().post(&url);

    // Add common headers
    client = client.header(CONTENT_TYPE, "application/json");

    // Add authorization header
    if let Some(api_key) = &chat_server.api_key
        && !api_key.is_empty()
    {
        client = client.header(AUTHORIZATION, api_key);
    } else if let Some(auth) = headers.get("authorization")
        && let Ok(auth_str) = auth.to_str()
    {
        client = client.header(AUTHORIZATION, auth_str);
    }

    dual_info!(
        "Request to downstream chat server - request_id: {}\n{}",
        request_id,
        serde_json::to_string_pretty(request).unwrap()
    );

    // Use select! to support cancellation
    select! {
        response = client.json(request).send() => {
            response.map_err(|e| ServerError::Operation(format!("Failed to forward request: {e}")))
        }
        _ = cancel_token.cancelled() => {
            let warn_msg = "Request was cancelled by client";
            dual_warn!("{}", warn_msg);
            Err(ServerError::Operation(warn_msg.to_string()))
        }
    }
}

/// Handle streaming chat responses, supporting tool calls and normal streaming responses
///
/// Choose processing path based on tool call identifier in response headers:
/// - Tool call needed: Extract tool call information from stream and call MCP server
/// - Normal streaming response: Directly process streaming data and return
///
/// # Arguments
///
/// * `response` - HTTP response from downstream server
/// * `request` - Chat request, may be modified
/// * `headers` - HTTP request headers
/// * `chat_service_url` - Chat service URL
/// * `request_id` - Request ID
/// * `cancel_token` - Cancellation token
async fn handle_stream_response(
    response: reqwest::Response,
    request: &mut ChatCompletionRequest,
    headers: &HeaderMap,
    chat_server: &TargetServerInfo,
    request_id: &str,
    cancel_token: CancellationToken,
) -> ServerResult<axum::response::Response> {
    let status = response.status();

    // check the status code
    match status {
        StatusCode::OK => {
            let response_headers = response.headers().clone();

            // Check if the response requires tool call
            let requires_tool_call = parse_requires_tool_call_header(&response_headers);

            if requires_tool_call {
                // Handle tool call in stream mode
                handle_tool_call_stream(
                    response,
                    request,
                    headers,
                    chat_server,
                    request_id,
                    cancel_token,
                )
                .await
            } else {
                // Handle normal response in stream mode
                handle_normal_stream(response, status, response_headers, request_id, cancel_token)
                    .await
            }
        }
        _ => {
            let err = response.error_for_status().unwrap_err();
            let err_msg = format!("{err}");

            dual_error!("{} - request_id: {}", err_msg, request_id);

            return Err(ServerError::Operation(err_msg));
        }
    }
}

/// Handle non-streaming chat responses, supporting both tool calls and normal responses
///
/// This function implements the following features:
/// 1. Read complete response data from downstream server (with cancellation support)
/// 2. Parse tool call identifier from response headers
/// 3. Choose different processing paths based on tool call requirements:
///    - Tool call needed: Parse response and call MCP server
///    - Normal response: Directly build and return response
/// 4. Provide complete error handling and cancellation support
///
/// # Arguments
///
/// * `response` - HTTP response object from downstream server
/// * `request` - Chat completion request, may be modified (e.g., add tool call results)
/// * `headers` - HTTP request headers for subsequent requests
/// * `chat_service_url` - Chat service URL for re-requesting after tool calls
/// * `request_id` - Request ID for log tracking and error handling
/// * `cancel_token` - Cancellation token for request cancellation support
///
/// # Returns
/// * `Ok(response)` - Successfully built HTTP response
/// * `Err(ServerError)` - Error occurred during processing or was cancelled
///
/// # Processing Flow
/// 1. Extract response status code and header information
/// 2. Read response body data (with cancellation support)
/// 3. Check if tool call is required
/// 4. Choose processing path based on tool call requirements:
///    - Tool call path: Parse response → Call MCP → Re-request → Return result
///    - Normal path: Directly build response and return
///
/// # Error Handling
/// * Response reading error: Return detailed error information
/// * Cancellation operation: Log warning and return cancellation error
/// * Tool call error: Decide whether to continue based on error type
/// * Response building error: Return build failure error
async fn handle_non_stream_response(
    response: reqwest::Response,
    request: &mut ChatCompletionRequest,
    headers: &HeaderMap,
    chat_server: &TargetServerInfo,
    request_id: &str,
    cancel_token: CancellationToken,
) -> ServerResult<axum::response::Response> {
    let status = response.status();

    // check the status code
    match status {
        StatusCode::OK => {
            let response_headers = response.headers().clone();

            // Read the response body
            let bytes = read_response_bytes(response, request_id, cancel_token.clone()).await?;
            let chat_completion = parse_chat_completion(&bytes, request_id)?;

            // Check if the response requires tool call
            let requires_tool_call = !chat_completion.choices[0].message.tool_calls.is_empty();

            if requires_tool_call {
                call_mcp_server(
                    chat_completion.choices[0].message.tool_calls.as_slice(),
                    request,
                    headers,
                    chat_server,
                    request_id,
                    cancel_token,
                )
                .await
            } else {
                // Handle normal response in non-stream mode
                build_response(status, response_headers, bytes, request_id)
            }
        }
        _ => {
            let err = response.error_for_status().unwrap_err();
            let err_msg = format!("{err}");

            dual_error!("{} - request_id: {}", err_msg, request_id);

            return Err(ServerError::Operation(err_msg));
        }
    }
}

/// Handle tool calls in streaming responses
///
/// Parse tool call information from streaming response, call MCP server to execute tools,
/// then add tool execution results to the request and re-send the request.
///
/// # Arguments
///
/// * `response` - HTTP response from downstream server
/// * `request` - Chat request, will be modified to include tool call results
/// * `headers` - HTTP request headers
/// * `chat_server` - Chat server information
/// * `request_id` - Request ID
/// * `cancel_token` - Cancellation token
async fn handle_tool_call_stream(
    response: reqwest::Response,
    request: &mut ChatCompletionRequest,
    headers: &HeaderMap,
    chat_server: &TargetServerInfo,
    request_id: &str,
    cancel_token: CancellationToken,
) -> ServerResult<axum::response::Response> {
    let tool_calls = extract_tool_calls_from_stream(response, request_id).await?;
    call_mcp_server(
        tool_calls.as_slice(),
        request,
        headers,
        chat_server,
        request_id,
        cancel_token,
    )
    .await
}

/// Parse tool call identifier from HTTP response headers
///
/// Check if the "requires-tool-call" field exists in response headers and parse it as boolean.
/// Returns false if the field doesn't exist or parsing fails.
fn parse_requires_tool_call_header(headers: &HeaderMap) -> bool {
    headers
        .get("requires-tool-call")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<bool>().ok())
        .unwrap_or(false)
}

async fn handle_normal_stream(
    response: reqwest::Response,
    status: StatusCode,
    response_headers: HeaderMap,
    request_id: &str,
    cancel_token: CancellationToken,
) -> ServerResult<axum::response::Response> {
    // Handle response body reading with cancellation
    let bytes = select! {
        bytes = response.bytes() => {
            bytes.map_err(|e| {
                let err_msg = format!("Failed to get the full response as bytes: {e}");
                dual_error!("{} - request_id: {}", err_msg, request_id);
                ServerError::Operation(err_msg)
            })?
        }
        _ = cancel_token.cancelled() => {
            let warn_msg = "Request was cancelled while reading response";
            dual_warn!("{} - request_id: {}", warn_msg, request_id);
            return Err(ServerError::Operation(warn_msg.to_string()));
        }
    };

    // build the response builder
    let response_builder = Response::builder().status(status);

    // copy the response headers
    let response_builder = copy_response_headers(response_builder, &response_headers);

    match response_builder.body(Body::from(bytes)) {
        Ok(response) => {
            dual_info!(
                "Chat request completed successfully - request_id: {}",
                request_id
            );
            Ok(response)
        }
        Err(e) => {
            let err_msg = format!("Failed to create the response: {e}");
            dual_error!("{} - request_id: {}", err_msg, request_id);
            Err(ServerError::Operation(err_msg))
        }
    }
}

/// Read HTTP response body data with cancellation support
///
/// This function uses select! macro to simultaneously monitor response reading and cancellation signals.
/// When the request is cancelled, it immediately returns an error to avoid resource waste.
async fn read_response_bytes(
    response: reqwest::Response,
    request_id: &str,
    cancel_token: CancellationToken,
) -> ServerResult<Bytes> {
    select! {
        bytes = response.bytes() => {
            bytes.map_err(|e| {
                let err_msg = format!("Failed to get the full response as bytes: {e}");
                dual_error!("{} - request_id: {}", err_msg, request_id);
                ServerError::Operation(err_msg)
            })
        }
        _ = cancel_token.cancelled() => {
            let warn_msg = "Request was cancelled while reading response";
            dual_warn!("{} - request_id: {}", warn_msg, request_id);
            Err(ServerError::Operation(warn_msg.to_string()))
        }
    }
}

/// Build HTTP response object
///
/// Build complete HTTP response based on status code, response headers and response body data.
/// Copy all response headers to the new response and log success message.
///
/// # Arguments
///
/// * `status` - HTTP status code
/// * `response_headers` - Response headers
/// * `bytes` - Response body data
/// * `request_id` - Request ID for logging
fn build_response(
    status: StatusCode,
    response_headers: HeaderMap,
    bytes: Bytes,
    request_id: &str,
) -> ServerResult<axum::response::Response> {
    // build the response builder
    let mut response_builder = Response::builder().status(status);

    // copy the response headers
    response_builder = copy_response_headers(response_builder, &response_headers);

    match response_builder.body(Body::from(bytes)) {
        Ok(response) => {
            dual_info!(
                "Chat request completed successfully - request_id: {}",
                request_id
            );

            Ok(response)
        }
        Err(e) => {
            let err_msg = format!("Failed to create the response: {e}");
            dual_error!("{} - request_id: {}", err_msg, request_id);
            Err(ServerError::Operation(err_msg))
        }
    }
}

/// Extract tool call information from streaming response
///
/// Parse streaming response data and extract tool call information.
/// Process SSE format data stream, parse ChatCompletionChunk and extract tool_calls.
async fn extract_tool_calls_from_stream(
    response: reqwest::Response,
    request_id: &str,
) -> ServerResult<Vec<ToolCall>> {
    let mut ds_stream = response.bytes_stream();
    let mut tool_calls: Vec<ToolCall> = Vec::new();

    while let Some(item) = ds_stream.next().await {
        match item {
            Ok(bytes) => {
                match String::from_utf8(bytes.to_vec()) {
                    Ok(s) => {
                        let x = s
                            .trim_start_matches("data:")
                            .trim()
                            .split("data:")
                            .collect::<Vec<_>>();
                        let s = x[0];

                        dual_debug!("s: {}", s);

                        // convert the bytes to ChatCompletionChunk
                        if let Ok(chunk) = serde_json::from_str::<ChatCompletionChunk>(s) {
                            dual_debug!("chunk: {:?} - request_id: {}", &chunk, request_id);

                            if !chunk.choices.is_empty() {
                                for tool in chunk.choices[0].delta.tool_calls.iter() {
                                    let tool_call = tool.clone().into();

                                    dual_debug!("tool_call: {:?}", &tool_call);

                                    tool_calls.push(tool_call);
                                }

                                break;
                            }
                        }
                    }
                    Err(e) => {
                        let err_msg = format!(
                            "Failed to convert bytes from downstream server into string: {e}"
                        );
                        dual_error!("{} - request_id: {}", err_msg, request_id);
                        return Err(ServerError::Operation(err_msg));
                    }
                }
            }
            Err(e) => {
                let err_msg = format!("Failed to get the full response as bytes: {e}");
                dual_error!("{} - request_id: {}", err_msg, request_id);
                return Err(ServerError::Operation(err_msg));
            }
        }
    }

    Ok(tool_calls)
}

fn parse_chat_completion(bytes: &Bytes, request_id: &str) -> ServerResult<ChatCompletionObject> {
    serde_json::from_slice(bytes).map_err(|e| {
        let value = serde_json::from_slice::<serde_json::Value>(bytes).unwrap();

        dual_error!(
            "The response body received from the downstream server - request_id: {}:\n{}",
            request_id,
            serde_json::to_string_pretty(&value).unwrap()
        );

        let err_msg = format!("Failed to parse the response: {e}");

        dual_error!("{} - request_id: {}", err_msg, request_id);

        ServerError::Operation(err_msg)
    })
}

/// Copy HTTP response headers to response builder
///
/// Selectively copy response headers based on whether it's a streaming response.
///
/// # Arguments
///
/// * `response_builder` - Response builder
/// * `headers` - Source response headers
fn copy_response_headers(
    response_builder: axum::http::response::Builder,
    headers: &HeaderMap,
) -> axum::http::response::Builder {
    let allowed_headers = [
        "access-control-allow-origin",
        "access-control-allow-headers",
        "access-control-allow-methods",
        "content-type",
        "content-length",
        "cache-control",
        "connection",
        "user",
        "date",
        "requires-tool-call",
    ];

    headers
        .iter()
        .fold(response_builder, |builder, (name, value)| {
            if allowed_headers.contains(&name.as_str()) {
                dual_debug!("copy header: {} - {}", name, value.to_str().unwrap());
                builder.header(name, value)
            } else {
                dual_debug!("ignore header: {} - {}", name, value.to_str().unwrap());
                builder
            }
        })
}

async fn call_mcp_server(
    tool_calls: &[ToolCall],
    request: &mut ChatCompletionRequest,
    headers: &HeaderMap,
    chat_server: &TargetServerInfo,
    request_id: impl AsRef<str>,
    cancel_token: CancellationToken,
) -> ServerResult<axum::response::Response> {
    let request_id = request_id.as_ref();
    // let chat_service_url = chat_service_url.as_ref();
    let chat_service_url = format!("{}/chat/completions", chat_server.url.trim_end_matches('/'));

    dual_debug!(
        "tool calls:\n{}",
        serde_json::to_string_pretty(tool_calls).unwrap()
    );
    dual_debug!(
        "first tool call:\n{}",
        serde_json::to_string_pretty(&tool_calls[0]).unwrap()
    );

    let tool_call = &tool_calls[0];
    let tool_call_id = tool_call.id.as_str();
    let tool_name = tool_call.function.name.as_str();
    let tool_args = &tool_call.function.arguments;

    dual_debug!(
        "tool name: {}, tool args: {} - request_id: {}",
        tool_name,
        tool_args,
        request_id
    );

    // convert the func_args to a json object
    let arguments =
        serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(tool_args).ok();

    // find mcp client by tool name
    if let Some(mcp_tools) = MCP_TOOLS.get() {
        let tools = mcp_tools.read().await;
        dual_debug!("mcp_tools: {:?}", mcp_tools);

        // look up the tool name in MCP_TOOLS
        if let Some(mcp_client_name) = tools.get(tool_name) {
            if let Some(services) = MCP_SERVICES.get() {
                let service_map = services.read().await;
                // get the mcp client
                let service = match service_map.get(mcp_client_name) {
                    Some(mcp_client) => mcp_client,
                    None => {
                        let err_msg = format!("Tool not found: {tool_name}");
                        dual_error!("{} - request_id: {}", err_msg, request_id);
                        return Err(ServerError::Operation(err_msg.to_string()));
                    }
                };

                // get the server name from the peer info
                let raw_server_name = match service.read().await.raw.peer_info() {
                    Some(peer_info) => {
                        let server_name = peer_info.server_info.name.clone();
                        dual_debug!(
                            "server name from peer info: {} - request_id: {}",
                            server_name,
                            request_id
                        );
                        server_name
                    }
                    None => {
                        dual_warn!(
                            "Failed to get peer info from the MCP client: {mcp_client_name}"
                        );

                        String::new()
                    }
                };

                dual_info!(
                    "Call `{}::{}` mcp tool - request_id: {}",
                    raw_server_name,
                    tool_name,
                    request_id
                );

                // call a tool
                let request_param = CallToolRequestParam {
                    name: tool_name.to_string().into(),
                    arguments,
                };
                let res = service
                    .read()
                    .await
                    .raw
                    .call_tool(request_param)
                    .await
                    .map_err(|e| {
                        dual_error!("Failed to call the tool: {}", e);
                        ServerError::Operation(e.to_string())
                    })?;
                dual_debug!("{}", serde_json::to_string_pretty(&res).unwrap());

                match res.is_error {
                    Some(false) => {
                        match res.content.is_empty() {
                            true => {
                                let err_msg = "The mcp tool result is empty";
                                dual_error!("{} - request_id: {}", err_msg, request_id);
                                Err(ServerError::McpEmptyContent)
                            }
                            false => {
                                let content = &res.content[0];
                                match &content.raw {
                                    RawContent::Text(text) => {
                                        dual_info!("The mcp tool call result: {:#?}", text.text);

                                        match SEARCH_MCP_SERVER_NAMES
                                            .contains(&raw_server_name.as_str())
                                        {
                                            true => {
                                                // get the fallback message from the mcp client
                                                let fallback = if service
                                                    .read()
                                                    .await
                                                    .has_fallback_message()
                                                {
                                                    service
                                                        .read()
                                                        .await
                                                        .fallback_message
                                                        .clone()
                                                        .unwrap()
                                                } else {
                                                    DEFAULT_SEARCH_FALLBACK_MESSAGE.to_string()
                                                };

                                                dual_debug!(
                                                    "fallback message: {} - request_id: {}",
                                                    fallback,
                                                    request_id
                                                );

                                                // format the content
                                                let content = format!(
                                                    "Please answer the question based on the information between **---BEGIN CONTEXT---** and **---END CONTEXT---**. Do not use any external knowledge. If the information between **---BEGIN CONTEXT---** and **---END CONTEXT---** is empty, please respond with `{fallback}`. Note that DO NOT use any tools if provided.\n\n---BEGIN CONTEXT---\n\n{context}\n\n---END CONTEXT---",
                                                    fallback = fallback,
                                                    context = &text.text,
                                                );

                                                // append assistant message with tool call to request messages
                                                let assistant_completion_message =
                                                    ChatCompletionRequestMessage::Assistant(
                                                        ChatCompletionAssistantMessage::new(
                                                            None,
                                                            None,
                                                            Some(tool_calls.to_vec()),
                                                        ),
                                                    );
                                                request.messages.push(assistant_completion_message);

                                                // append tool message with tool result to request messages
                                                let tool_completion_message =
                                                    ChatCompletionRequestMessage::Tool(
                                                        ChatCompletionToolMessage::new(
                                                            &content,
                                                            Some(tool_call_id.to_string()),
                                                        ),
                                                    );
                                                request.messages.push(tool_completion_message);

                                                // disable tool choice
                                                if request.tool_choice.is_some() {
                                                    request.tool_choice = Some(ToolChoice::None);
                                                }

                                                // Create a request client that can be cancelled
                                                let ds_request = if let Some(api_key) =
                                                    &chat_server.api_key
                                                    && !api_key.is_empty()
                                                {
                                                    reqwest::Client::new()
                                                        .post(&chat_service_url)
                                                        .header(CONTENT_TYPE, "application/json")
                                                        .header(AUTHORIZATION, api_key)
                                                        .json(&request)
                                                } else if headers.contains_key("authorization") {
                                                    let authorization = headers
                                                        .get("authorization")
                                                        .unwrap()
                                                        .to_str()
                                                        .unwrap()
                                                        .to_string();

                                                    reqwest::Client::new()
                                                        .post(&chat_service_url)
                                                        .header(CONTENT_TYPE, "application/json")
                                                        .header(AUTHORIZATION, authorization)
                                                        .json(&request)
                                                } else {
                                                    reqwest::Client::new()
                                                        .post(&chat_service_url)
                                                        .header(CONTENT_TYPE, "application/json")
                                                        .json(&request)
                                                };

                                                dual_info!(
                                                    "Request to downstream chat server - request_id: {}\n{}",
                                                    request_id,
                                                    serde_json::to_string_pretty(&request).unwrap()
                                                );

                                                // Use select! to handle request cancellation
                                                let ds_response = select! {
                                                    response = ds_request.send() => {
                                                        response.map_err(|e| {
                                                            let err_msg = format!(
                                                                "Failed to forward the request to the downstream server: {e}"
                                                            );
                                                            dual_error!("{} - request_id: {}", err_msg, request_id);
                                                            ServerError::Operation(err_msg)
                                                        })?
                                                    }
                                                    _ = cancel_token.cancelled() => {
                                                        let warn_msg = "Request was cancelled by client";
                                                        dual_warn!("{} - request_id: {}", warn_msg, request_id);
                                                        return Err(ServerError::Operation(warn_msg.to_string()));
                                                    }
                                                };

                                                let status = ds_response.status();
                                                let headers = ds_response.headers().clone();

                                                // Handle response body reading with cancellation
                                                let bytes = select! {
                                                    bytes = ds_response.bytes() => {
                                                        bytes.map_err(|e| {
                                                            let err_msg = format!("Failed to get the full response as bytes: {e}");
                                                            dual_error!("{} - request_id: {}", err_msg, request_id);
                                                            ServerError::Operation(err_msg)
                                                        })?
                                                    }
                                                    _ = cancel_token.cancelled() => {
                                                        let warn_msg = "Request was cancelled while reading response";
                                                        dual_warn!("{} - request_id: {}", warn_msg, request_id);
                                                        return Err(ServerError::Operation(warn_msg.to_string()));
                                                    }
                                                };

                                                let mut response_builder =
                                                    Response::builder().status(status);

                                                // Copy all headers from downstream response
                                                match request.stream {
                                                    Some(true) => {
                                                        for (name, value) in headers.iter() {
                                                            match name.as_str() {
                                                                "access-control-allow-origin" => {
                                                                    response_builder =
                                                                        response_builder
                                                                            .header(name, value);
                                                                }
                                                                "access-control-allow-headers" => {
                                                                    response_builder =
                                                                        response_builder
                                                                            .header(name, value);
                                                                }
                                                                "access-control-allow-methods" => {
                                                                    response_builder =
                                                                        response_builder
                                                                            .header(name, value);
                                                                }
                                                                "content-type" => {
                                                                    response_builder =
                                                                        response_builder
                                                                            .header(name, value);
                                                                }
                                                                "cache-control" => {
                                                                    response_builder =
                                                                        response_builder
                                                                            .header(name, value);
                                                                }
                                                                "connection" => {
                                                                    response_builder =
                                                                        response_builder
                                                                            .header(name, value);
                                                                }
                                                                "user" => {
                                                                    response_builder =
                                                                        response_builder
                                                                            .header(name, value);
                                                                }
                                                                "date" => {
                                                                    response_builder =
                                                                        response_builder
                                                                            .header(name, value);
                                                                }
                                                                _ => {
                                                                    dual_debug!(
                                                                        "ignore header: {} - {}",
                                                                        name,
                                                                        value.to_str().unwrap()
                                                                    );
                                                                }
                                                            }
                                                        }
                                                    }
                                                    Some(false) | None => {
                                                        for (name, value) in headers.iter() {
                                                            dual_debug!(
                                                                "{}: {}",
                                                                name,
                                                                value.to_str().unwrap()
                                                            );
                                                            response_builder = response_builder
                                                                .header(name, value);
                                                        }
                                                    }
                                                }

                                                match response_builder.body(Body::from(bytes)) {
                                                    Ok(response) => {
                                                        dual_info!(
                                                            "Chat request completed successfully - request_id: {}",
                                                            request_id
                                                        );
                                                        Ok(response)
                                                    }
                                                    Err(e) => {
                                                        let err_msg = format!(
                                                            "Failed to create the response: {e}"
                                                        );
                                                        dual_error!(
                                                            "{} - request_id: {}",
                                                            err_msg,
                                                            request_id
                                                        );
                                                        Err(ServerError::Operation(err_msg))
                                                    }
                                                }
                                            }
                                            false => {
                                                // create an assistant message
                                                let tool_completion_message =
                                                    ChatCompletionRequestMessage::Tool(
                                                        ChatCompletionToolMessage::new(
                                                            &text.text,
                                                            Some(tool_call_id.to_string()),
                                                        ),
                                                    );

                                                // append assistant message with tool call to request messages
                                                let assistant_completion_message =
                                                    ChatCompletionRequestMessage::Assistant(
                                                        ChatCompletionAssistantMessage::new(
                                                            None,
                                                            None,
                                                            Some(tool_calls.to_vec()),
                                                        ),
                                                    );
                                                request.messages.push(assistant_completion_message);
                                                // append tool message with tool result to request messages
                                                request.messages.push(tool_completion_message);

                                                // disable tool choice
                                                if request.tool_choice.is_some() {
                                                    request.tool_choice = Some(ToolChoice::None);
                                                }

                                                // Create a request client that can be cancelled
                                                let ds_request = if let Some(api_key) =
                                                    &chat_server.api_key
                                                    && !api_key.is_empty()
                                                {
                                                    reqwest::Client::new()
                                                        .post(&chat_service_url)
                                                        .header(CONTENT_TYPE, "application/json")
                                                        .header(AUTHORIZATION, api_key)
                                                        .json(&request)
                                                } else if headers.contains_key("authorization") {
                                                    let authorization = headers
                                                        .get("authorization")
                                                        .unwrap()
                                                        .to_str()
                                                        .unwrap()
                                                        .to_string();

                                                    reqwest::Client::new()
                                                        .post(&chat_service_url)
                                                        .header(CONTENT_TYPE, "application/json")
                                                        .header(AUTHORIZATION, authorization)
                                                        .json(&request)
                                                } else {
                                                    reqwest::Client::new()
                                                        .post(&chat_service_url)
                                                        .header(CONTENT_TYPE, "application/json")
                                                        .json(&request)
                                                };

                                                dual_info!(
                                                    "Request to downstream chat server - request_id: {}\n{}",
                                                    request_id,
                                                    serde_json::to_string_pretty(&request).unwrap()
                                                );

                                                // Use select! to handle request cancellation
                                                let ds_response = select! {
                                                    response = ds_request.send() => {
                                                        response.map_err(|e| {
                                                            let err_msg = format!(
                                                                "Failed to forward the request to the downstream server: {e}"
                                                            );
                                                            dual_error!("{} - request_id: {}", err_msg, request_id);
                                                            ServerError::Operation(err_msg)
                                                        })?
                                                    }
                                                    _ = cancel_token.cancelled() => {
                                                        let warn_msg = "Request was cancelled by client";
                                                        dual_warn!("{} - request_id: {}", warn_msg, request_id);
                                                        return Err(ServerError::Operation(warn_msg.to_string()));
                                                    }
                                                };

                                                let status = ds_response.status();
                                                let mut response_builder =
                                                    Response::builder().status(status);

                                                // copy the response headers
                                                response_builder = copy_response_headers(
                                                    response_builder,
                                                    ds_response.headers(),
                                                );

                                                // Handle response body reading with cancellation
                                                let bytes = select! {
                                                    bytes = ds_response.bytes() => {
                                                        bytes.map_err(|e| {
                                                            let err_msg = format!("Failed to get the full response as bytes: {e}");
                                                            dual_error!("{} - request_id: {}", err_msg, request_id);
                                                            ServerError::Operation(err_msg)
                                                        })?
                                                    }
                                                    _ = cancel_token.cancelled() => {
                                                        let warn_msg = "Request was cancelled while reading response";
                                                        dual_warn!("{} - request_id: {}", warn_msg, request_id);
                                                        return Err(ServerError::Operation(warn_msg.to_string()));
                                                    }
                                                };

                                                match response_builder.body(Body::from(bytes)) {
                                                    Ok(response) => {
                                                        dual_info!(
                                                            "Chat request completed successfully - request_id: {}",
                                                            request_id
                                                        );
                                                        Ok(response)
                                                    }
                                                    Err(e) => {
                                                        let err_msg = format!(
                                                            "Failed to create the response: {e}"
                                                        );
                                                        dual_error!(
                                                            "{} - request_id: {}",
                                                            err_msg,
                                                            request_id
                                                        );
                                                        Err(ServerError::Operation(err_msg))
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    _ => {
                                        let err_msg =
                                            "Only text content is supported for tool call results";
                                        dual_error!("{} - request_id: {}", err_msg, request_id);
                                        Err(ServerError::Operation(err_msg.to_string()))
                                    }
                                }
                            }
                        }
                    }
                    _ => {
                        let err_msg = format!("Failed to call the tool: {tool_name}");
                        dual_error!("{} - request_id: {}", err_msg, request_id);
                        Err(ServerError::Operation(err_msg))
                    }
                }
            } else {
                let err_msg = "Empty MCP CLIENTS";
                dual_error!("{} - request_id: {}", err_msg, request_id);
                Err(ServerError::Operation(err_msg.to_string()))
            }
        } else {
            let err_msg = format!("Failed to find the MCP client with tool name: {tool_name}");
            dual_error!("{} - request_id: {}", err_msg, request_id);
            Err(ServerError::McpNotFoundClient)
        }
    } else {
        let err_msg = "Empty MCP TOOLS";
        dual_error!("{} - request_id: {}", err_msg, request_id);
        Err(ServerError::Operation(err_msg.to_string()))
    }
}

pub(crate) mod responses {
    use super::*;
    use crate::{
        responses::{ResponseRequest, ResponseObject, DeleteResponseResult, InputItemList},
    };
    use axum::extract::Path;
    use chrono::Utc;

    pub(crate) async fn create_response_handler(
        State(state): State<Arc<AppState>>,
        Extension(cancel_token): Extension<CancellationToken>,
        headers: HeaderMap,
        Json(request): Json<ResponseRequest>,
    ) -> ServerResult<axum::response::Response> {
        let request_id = headers
            .get("x-request-id")
            .and_then(|h| h.to_str().ok())
            .unwrap_or("unknown")
            .to_string();

        dual_info!(
            "Received a new response request for model: {} - request_id: {}",
            request.model,
            request_id
        );

        let response_id = ResponseRequest::generate_id();
        let _created_at = Utc::now().timestamp();

        // Get conversation history if previous_response_id is provided
        let conversation_history = if let Some(prev_id) = &request.previous_response_id {
            match build_conversation_history(&state.database, prev_id.clone(), &request_id).await {
                Ok(history) => history,
                Err(e) => {
                    dual_warn!("Failed to build conversation history for {}: {} - request_id: {}", prev_id, e, request_id);
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        };

        // Convert ResponseRequest to ChatCompletionRequest
        let chat_request = request.to_chat_completion_request(conversation_history);

        // Get target chat server
        let chat_server = get_chat_server(&state, &request_id).await?;

        // Send request to downstream server
        let mut mutable_chat_request = chat_request;
        let response = send_request_with_retry(
            &chat_server,
            &mut mutable_chat_request,
            &headers,
            &request_id,
            cancel_token.clone(),
        )
        .await?;

        // Handle the response based on stream mode
        let response_obj = match request.stream.unwrap_or(false) {
            true => {
                // For streaming responses, we need different handling
                // For now, return an error as streaming needs special implementation
                return Err(ServerError::Operation("Streaming responses not yet implemented".to_string()));
            }
            false => {
                // Handle non-stream response
                let bytes = read_response_bytes(response, &request_id, cancel_token).await?;
                let chat_completion = parse_chat_completion(&bytes, &request_id)?;
                
                // Convert to ResponseObject
                let mut response_obj = ResponseObject::from(chat_completion);
                response_obj.id = response_id.clone();
                response_obj.previous_response_id = request.previous_response_id.clone();
                response_obj.instructions = request.instructions.clone();
                response_obj.temperature = request.temperature;
                response_obj.top_p = request.top_p;
                response_obj.store = request.store;
                response_obj.metadata = request.metadata.clone();
                response_obj.user = request.user.clone();
                response_obj.safety_identifier = request.safety_identifier.clone();
                response_obj.prompt_cache_key = request.prompt_cache_key.clone();
                response_obj.truncation = request.truncation.clone();
                response_obj.verbosity = request.verbosity.clone();

                response_obj
            }
        };

        // Store response in database
        let db_response = crate::database::ResponseSession {
            id: response_obj.id.clone(),
            object: response_obj.object.clone(),
            created_at: response_obj.created_at,
            status: response_obj.status.clone(),
            model: response_obj.model.clone(),
            previous_response_id: response_obj.previous_response_id.clone(),
            instructions: response_obj.instructions.clone(),
            max_output_tokens: response_obj.max_output_tokens,
            temperature: response_obj.temperature,
            top_p: response_obj.top_p,
            store: response_obj.store.unwrap_or(true),
            metadata: response_obj.metadata.clone(),
            user_id: response_obj.user.clone(),
            safety_identifier: response_obj.safety_identifier.clone(),
            prompt_cache_key: response_obj.prompt_cache_key.clone(),
            usage_input_tokens: response_obj.usage.as_ref().map(|u| u.input_tokens),
            usage_output_tokens: response_obj.usage.as_ref().map(|u| u.output_tokens),
            usage_total_tokens: response_obj.usage.as_ref().map(|u| u.total_tokens),
            error: response_obj.error.as_ref().map(|e| serde_json::to_string(e).unwrap_or_default()),
            incomplete_details: response_obj.incomplete_details.as_ref().map(|d| serde_json::to_string(d).unwrap_or_default()),
        };

        if let Err(e) = state.database.store_response(db_response).await {
            dual_warn!("Failed to store response in database: {} - request_id: {}", e, request_id);
        } else {
            // Store input items
            if let Some(input) = &request.input {
                let input_content = serde_json::to_string(input).unwrap_or_default();
                let input_item = crate::database::InputItem {
                    id: ResponseRequest::generate_message_id(),
                    response_id: response_obj.id.clone(),
                    item_type: "message".to_string(),
                    role: Some(Role::User),
                    content: input_content,
                    created_at: response_obj.created_at,
                };
                
                if let Err(e) = state.database.store_input_item(input_item).await {
                    dual_warn!("Failed to store input item in database: {} - request_id: {}", e, request_id);
                }
            }
            
            // Store output items
            for output_item in &response_obj.output {
                let output_content = serde_json::to_string(output_item).unwrap_or_default();
                let db_output_item = crate::database::OutputItem {
                    id: output_item.id.clone(),
                    response_id: response_obj.id.clone(),
                    item_type: output_item.item_type.clone(),
                    role: output_item.role.clone(),
                    content: output_content,
                    status: output_item.status.clone(),
                    created_at: response_obj.created_at,
                };
                
                if let Err(e) = state.database.store_output_item(db_output_item).await {
                    dual_warn!("Failed to store output item in database: {} - request_id: {}", e, request_id);
                }
            }
        }

        let json_body = serde_json::to_string(&response_obj).map_err(|e| {
            let err_msg = format!("Failed to serialize response: {}", e);
            dual_error!("{} - request_id: {}", err_msg, request_id);
            ServerError::Operation(err_msg)
        })?;

        Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "application/json")
            .body(Body::from(json_body))
            .map_err(|e| {
                let err_msg = format!("Failed to create response: {}", e);
                dual_error!("{} - request_id: {}", err_msg, request_id);
                ServerError::Operation(err_msg)
            })
    }

    pub(crate) async fn get_response_handler(
        State(state): State<Arc<AppState>>,
        headers: HeaderMap,
        Path(response_id): Path<String>,
    ) -> ServerResult<axum::response::Response> {
        let request_id = headers
            .get("x-request-id")
            .and_then(|h| h.to_str().ok())
            .unwrap_or("unknown")
            .to_string();

        dual_info!(
            "Getting response: {} - request_id: {}",
            response_id,
            request_id
        );

        match state.database.get_response(&response_id).await {
            Ok(Some(db_response)) => {
                // Convert database response to API response format
                let response_obj = ResponseObject {
                    id: db_response.id,
                    object: db_response.object,
                    created_at: db_response.created_at,
                    model: db_response.model,
                    status: db_response.status,
                    previous_response_id: db_response.previous_response_id,
                    instructions: db_response.instructions,
                    max_output_tokens: db_response.max_output_tokens,
                    temperature: db_response.temperature,
                    top_p: db_response.top_p,
                    store: Some(db_response.store),
                    metadata: db_response.metadata,
                    user: db_response.user_id,
                    safety_identifier: db_response.safety_identifier,
                    prompt_cache_key: db_response.prompt_cache_key,
                    tools: None, // TODO: Implement tool retrieval
                    tool_choice: None, // TODO: Implement tool choice retrieval
                    parallel_tool_calls: None,
                    output: Vec::new(), // TODO: Get from output_items table
                    error: db_response.error.and_then(|e| serde_json::from_str(&e).ok()),
                    incomplete_details: db_response.incomplete_details.and_then(|d| serde_json::from_str(&d).ok()),
                    usage: if let (Some(input), Some(output), Some(total)) = 
                        (db_response.usage_input_tokens, db_response.usage_output_tokens, db_response.usage_total_tokens) {
                        Some(crate::responses::Usage {
                            input_tokens: input,
                            output_tokens: output,
                            total_tokens: total,
                            input_tokens_details: None,
                            output_tokens_details: None,
                        })
                    } else {
                        None
                    },
                    reasoning: None,
                    truncation: None,
                    verbosity: None,
                };

                let json_body = serde_json::to_string(&response_obj).map_err(|e| {
                    let err_msg = format!("Failed to serialize response: {}", e);
                    dual_error!("{} - request_id: {}", err_msg, request_id);
                    ServerError::Operation(err_msg)
                })?;

                Response::builder()
                    .status(StatusCode::OK)
                    .header("Content-Type", "application/json")
                    .body(Body::from(json_body))
                    .map_err(|e| {
                        let err_msg = format!("Failed to create response: {}", e);
                        dual_error!("{} - request_id: {}", err_msg, request_id);
                        ServerError::Operation(err_msg)
                    })
            }
            Ok(None) => {
                Err(ServerError::Operation("Response not found".to_string()))
            }
            Err(e) => {
                let err_msg = format!("Database error: {}", e);
                dual_error!("{} - request_id: {}", err_msg, request_id);
                Err(ServerError::Operation(err_msg))
            }
        }
    }

    pub(crate) async fn delete_response_handler(
        State(state): State<Arc<AppState>>,
        headers: HeaderMap,
        Path(response_id): Path<String>,
    ) -> ServerResult<axum::response::Response> {
        let request_id = headers
            .get("x-request-id")
            .and_then(|h| h.to_str().ok())
            .unwrap_or("unknown")
            .to_string();

        dual_info!(
            "Deleting response: {} - request_id: {}",
            response_id,
            request_id
        );

        match state.database.delete_response(&response_id).await {
            Ok(deleted) => {
                if deleted {
                    let result = DeleteResponseResult {
                        id: response_id,
                        object: "response".to_string(),
                        deleted: true,
                    };

                    let json_body = serde_json::to_string(&result).map_err(|e| {
                        let err_msg = format!("Failed to serialize delete result: {}", e);
                        dual_error!("{} - request_id: {}", err_msg, request_id);
                        ServerError::Operation(err_msg)
                    })?;

                    Response::builder()
                        .status(StatusCode::OK)
                        .header("Content-Type", "application/json")
                        .body(Body::from(json_body))
                        .map_err(|e| {
                            let err_msg = format!("Failed to create response: {}", e);
                            dual_error!("{} - request_id: {}", err_msg, request_id);
                            ServerError::Operation(err_msg)
                        })
                } else {
                    Err(ServerError::Operation("Response not found".to_string()))
                }
            }
            Err(e) => {
                let err_msg = format!("Database error: {}", e);
                dual_error!("{} - request_id: {}", err_msg, request_id);
                Err(ServerError::Operation(err_msg))
            }
        }
    }

    pub(crate) async fn cancel_response_handler(
        State(_state): State<Arc<AppState>>,
        headers: HeaderMap,
        Path(response_id): Path<String>,
    ) -> ServerResult<axum::response::Response> {
        let request_id = headers
            .get("x-request-id")
            .and_then(|h| h.to_str().ok())
            .unwrap_or("unknown")
            .to_string();

        dual_info!(
            "Cancelling response: {} - request_id: {}",
            response_id,
            request_id
        );

        // TODO: Implement response cancellation
        // For now, return not found
        Err(ServerError::Operation("Response cancellation not yet implemented".to_string()))
    }

    pub(crate) async fn list_input_items_handler(
        State(_state): State<Arc<AppState>>,
        headers: HeaderMap,
        Path(response_id): Path<String>,
    ) -> ServerResult<axum::response::Response> {
        let request_id = headers
            .get("x-request-id")
            .and_then(|h| h.to_str().ok())
            .unwrap_or("unknown")
            .to_string();

        dual_info!(
            "Listing input items for response: {} - request_id: {}",
            response_id,
            request_id
        );

        // TODO: Implement input items listing from database
        // For now, return empty list
        let result = InputItemList {
            object: "list".to_string(),
            data: Vec::new(),
            first_id: "".to_string(),
            last_id: "".to_string(),
            has_more: false,
        };

        let json_body = serde_json::to_string(&result).map_err(|e| {
            let err_msg = format!("Failed to serialize input items: {}", e);
            dual_error!("{} - request_id: {}", err_msg, request_id);
            ServerError::Operation(err_msg)
        })?;

        Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "application/json")
            .body(Body::from(json_body))
            .map_err(|e| {
                let err_msg = format!("Failed to create response: {}", e);
                dual_error!("{} - request_id: {}", err_msg, request_id);
                ServerError::Operation(err_msg)
            })
    }

    async fn build_conversation_history(
        database: &Arc<crate::database::DatabaseManager>,
        previous_response_id: String,
        request_id: &str,
    ) -> Result<Vec<endpoints::chat::ChatCompletionRequestMessage>, crate::error::ServerError> {
        let mut messages = Vec::new();

        // Get the conversation history from database
        let response_sessions = match database.get_conversation_history(&previous_response_id).await {
            Ok(history) => {
                dual_debug!("Retrieved {} response sessions from conversation history - request_id: {}", history.len(), request_id);
                history
            }
            Err(e) => {
                return Err(crate::error::ServerError::Operation(format!("Failed to get conversation history: {}", e)));
            }
        };

        // For each response in history, get its input and output items
        for response in response_sessions {
            // Add system message from instructions if present
            if let Some(instructions) = &response.instructions {
                messages.push(endpoints::chat::ChatCompletionRequestMessage::System(
                    endpoints::chat::ChatCompletionSystemMessage::new(instructions.clone(), None)
                ));
            }

            // Get and add input items (user messages)
            match database.get_input_items(&response.id).await {
                Ok(input_items) => {
                    for input_item in input_items {
                        // Parse the stored JSON content back to InputTypes
                        if let Ok(input_content) = serde_json::from_str::<crate::responses::InputTypes>(&input_item.content) {
                            match input_content {
                                crate::responses::InputTypes::Text(text) => {
                                    let role = input_item.role.as_ref().unwrap_or(&Role::User);
                                    match role {
                                        Role::User => {
                                            messages.push(endpoints::chat::ChatCompletionRequestMessage::User(
                                                endpoints::chat::ChatCompletionUserMessage::new(
                                                    endpoints::chat::ChatCompletionUserMessageContent::Text(text),
                                                    None
                                                )
                                            ));
                                        }
                                        Role::System => {
                                            messages.push(endpoints::chat::ChatCompletionRequestMessage::System(
                                                endpoints::chat::ChatCompletionSystemMessage::new(text, None)
                                            ));
                                        }
                                        _ => {
                                            // Default to user message
                                            messages.push(endpoints::chat::ChatCompletionRequestMessage::User(
                                                endpoints::chat::ChatCompletionUserMessage::new(
                                                    endpoints::chat::ChatCompletionUserMessageContent::Text(text),
                                                    None
                                                )
                                            ));
                                        }
                                    }
                                }
                                crate::responses::InputTypes::Array(items) => {
                                    for item in items {
                                        match &item.content {
                                            crate::responses::InputContent::Text { text } => {
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
                                                // Skip non-text inputs for now
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    dual_warn!("Failed to get input items for response {}: {} - request_id: {}", response.id, e, request_id);
                }
            }

            // Get and add output items (assistant messages)
            match database.get_output_items(&response.id).await {
                Ok(output_items) => {
                    for output_item in output_items {
                        // Parse the stored JSON content back to OutputItem
                        if let Ok(parsed_output) = serde_json::from_str::<crate::responses::OutputItem>(&output_item.content) {
                            if let Some(content) = parsed_output.content {
                                for content_item in content {
                                    match content_item.content_data {
                                        crate::responses::OutputContentData::Text { text, .. } => {
                                            messages.push(endpoints::chat::ChatCompletionRequestMessage::Assistant(
                                                endpoints::chat::ChatCompletionAssistantMessage::new(Some(text), None, None)
                                            ));
                                        }
                                        _ => {
                                            // Skip non-text outputs for now
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    dual_warn!("Failed to get output items for response {}: {} - request_id: {}", response.id, e, request_id);
                }
            }
        }

        dual_debug!("Built conversation history with {} messages - request_id: {}", messages.len(), request_id);
        Ok(messages)
    }
}
