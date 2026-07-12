//! MCP tool definitions + dispatch. Each tool authenticates via the request's
//! `AuthContext` and forwards to the `Engine`.

use memoricai_core::dto::{
    ForgetRequest, IngestRequest, MemorySearchRequest, ProfileRequest, SearchInclude,
};
use memoricai_core::model::AuthContext;
use memoricai_core::Error;
use serde_json::{json, Value};

use crate::format::recall_markdown;
use crate::server::McpState;
use crate::{format, FORGET_THRESHOLD, MAX_CONTENT};

/// `tools/list` payload.
pub fn list() -> Value {
    json!({ "tools": [
        {
            "name": "memory",
            "description": "Save a memory, or forget one. Use action='save' to remember, 'forget' to remove.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "content": {"type": "string", "description": "The content to remember or forget"},
                    "action": {"type": "string", "enum": ["save", "forget"], "default": "save"},
                    "containerTag": {"type": "string", "description": "Project/container scope"}
                },
                "required": ["content"]
            }
        },
        {
            "name": "recall",
            "description": "Search memories and profile for relevant context.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {"type": "string"},
                    "includeProfile": {"type": "boolean", "default": true},
                    "containerTag": {"type": "string"}
                },
                "required": ["query"]
            }
        },
        {
            "name": "listProjects",
            "description": "List the available projects (container tags).",
            "inputSchema": {"type": "object", "properties": {}}
        },
        {
            "name": "whoAmI",
            "description": "Return the authenticated user's identity.",
            "inputSchema": {"type": "object", "properties": {}}
        },
        {
            "name": "memory-graph",
            "description": "Fetch a summary of the memory graph for a project.",
            "inputSchema": {"type": "object", "properties": {"containerTag": {"type": "string"}}}
        },
        {
            "name": "fetch-graph-data",
            "description": "Paginated graph data (used by the graph UI app).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "containerTag": {"type": "string"},
                    "page": {"type": "integer"},
                    "limit": {"type": "integer"}
                }
            },
            "_meta": {"ui": {"visibility": ["app"]}}
        }
    ]})
}

fn effective_tag(
    state: &McpState,
    auth: &AuthContext,
    arg: &Value,
    header_tag: Option<&str>,
) -> Result<String, String> {
    let requested = arg
        .get("containerTag")
        .and_then(|v| v.as_str())
        .or(header_tag);
    state
        .auth
        .scope_tag(auth, requested)
        .map(|tag| tag.unwrap_or_else(|| memoricai_core::DEFAULT_CONTAINER_TAG.to_string()))
        .map_err(|error| error.to_string())
}

fn text_result(text: impl Into<String>) -> Value {
    json!({ "content": [{"type": "text", "text": text.into()}] })
}

fn error_result(msg: impl Into<String>) -> Value {
    json!({ "content": [{"type": "text", "text": msg.into()}], "isError": true })
}

fn ingest_request(content: String, tag: String) -> IngestRequest {
    IngestRequest {
        content,
        custom_id: None,
        container_tag: Some(tag),
        container_tags: None,
        metadata: Some(json!({"mc_source": "mcp"})),
        entity_context: None,
        content_type: None,
        title: None,
        raw: None,
    }
}

fn search_request(q: String, tag: String, limit: u32, threshold: f32) -> MemorySearchRequest {
    MemorySearchRequest {
        q,
        container_tag: Some(tag),
        search_mode: "hybrid".into(),
        limit,
        threshold,
        rerank: false,
        rewrite_query: false,
        filters: None,
        include: SearchInclude {
            documents: true,
            related_memories: true,
            forgotten_memories: false,
        },
        digest: false,
    }
}

/// Dispatch a `tools/call`.
pub async fn call(
    state: &McpState,
    auth: &AuthContext,
    header_tag: Option<&str>,
    name: &str,
    args: &Value,
) -> Value {
    match name {
        "memory" => memory_tool(state, auth, header_tag, args).await,
        "recall" => recall_tool(state, auth, header_tag, args).await,
        "listProjects" => list_projects_tool(state, auth).await,
        "whoAmI" => who_am_i_tool(auth),
        "memory-graph" | "fetch-graph-data" => {
            memory_graph_tool(state, auth, header_tag, args).await
        }
        other => error_result(format!("unknown tool: {other}")),
    }
}

async fn memory_tool(
    state: &McpState,
    auth: &AuthContext,
    header_tag: Option<&str>,
    args: &Value,
) -> Value {
    let content = args.get("content").and_then(|c| c.as_str()).unwrap_or("");
    if content.is_empty() {
        return error_result("content is required");
    }
    if content.len() > MAX_CONTENT {
        return error_result("content exceeds maximum length");
    }
    if let Err(error) = state.auth.authorize_write(auth) {
        return error_result(error.to_string());
    }
    let action = args
        .get("action")
        .and_then(|a| a.as_str())
        .unwrap_or("save");
    let tag = match effective_tag(state, auth, args, header_tag) {
        Ok(tag) => tag,
        Err(error) => return error_result(error),
    };

    if action == "forget" {
        return forget_action(state, auth, content, &tag).await;
    }

    let req = ingest_request(content.to_string(), tag.clone());
    match state
        .engine
        .ingest(&auth.org.id, Some(&auth.user.id), &req)
        .await
    {
        Ok((id, _status)) => text_result(format!("Saved memory (id: {id}) in {tag} project")),
        Err(e) => error_result(format!("save failed: {e}")),
    }
}

async fn forget_action(state: &McpState, auth: &AuthContext, content: &str, tag: &str) -> Value {
    let direct = ForgetRequest {
        id: None,
        content: Some(content.to_string()),
        container_tag: tag.to_string(),
        reason: None,
    };
    match state.engine.forget(&auth.org.id, &direct).await {
        Ok(m) => text_result(format!("Forgot memory (id: {})", m.id)),
        Err(Error::NotFound(_)) => {
            // Semantic fallback: find the closest memory and forget by id.
            let sreq = search_request(content.to_string(), tag.to_string(), 5, FORGET_THRESHOLD);
            match state
                .engine
                .search_memories(&auth.org.id, &sreq, Some(tag))
                .await
            {
                Ok(res) => {
                    if let Some(hit) = res.results.into_iter().find(|r| r.memory.is_some()) {
                        let by_id = ForgetRequest {
                            id: Some(hit.id),
                            content: None,
                            container_tag: tag.to_string(),
                            reason: None,
                        };
                        match state.engine.forget(&auth.org.id, &by_id).await {
                            Ok(m) => text_result(format!("Forgot memory (id: {})", m.id)),
                            Err(e) => error_result(format!("forget failed: {e}")),
                        }
                    } else {
                        error_result("no matching memory to forget")
                    }
                }
                Err(e) => error_result(format!("forget lookup failed: {e}")),
            }
        }
        Err(e) => error_result(format!("forget failed: {e}")),
    }
}

async fn recall_tool(
    state: &McpState,
    auth: &AuthContext,
    header_tag: Option<&str>,
    args: &Value,
) -> Value {
    let query = args.get("query").and_then(|q| q.as_str()).unwrap_or("");
    if query.is_empty() {
        return error_result("query is required");
    }
    let include_profile = args
        .get("includeProfile")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let tag = match effective_tag(state, auth, args, header_tag) {
        Ok(tag) => tag,
        Err(error) => return error_result(error),
    };

    let sreq = search_request(query.to_string(), tag.clone(), 10, 0.3);
    let search_fut = state
        .engine
        .search_memories(&auth.org.id, &sreq, Some(&tag));
    let profile_fut = async {
        if include_profile {
            let preq = ProfileRequest {
                container_tag: tag.clone(),
                q: None,
                threshold: None,
                filters: None,
                include: None,
                buckets: None,
            };
            state
                .engine
                .profile(&auth.org.id, &preq)
                .await
                .ok()
                .map(|p| p.profile)
        } else {
            None
        }
    };
    let joined = tokio::try_join!(search_fut, async { Ok::<_, Error>(profile_fut.await) });
    let (search, profile) = match joined {
        Ok(r) => r,
        Err(e) => return error_result(format!("recall failed: {e}")),
    };

    let md = recall_markdown(profile.as_ref(), &search.results);
    text_result(md)
}

async fn list_projects_tool(state: &McpState, auth: &AuthContext) -> Value {
    match state.engine.db.list_spaces(&auth.org.id).await {
        Ok(spaces) => {
            let allowed = state.auth.allowed_container_tags(auth);
            let tags: Vec<String> = spaces
                .into_iter()
                .filter(|space| {
                    allowed
                        .as_ref()
                        .is_none_or(|tags| tags.iter().any(|tag| tag == &space.container_tag))
                })
                .map(|s| s.container_tag)
                .collect();
            json!({
                "content": [{"type": "text", "text": format!("Projects: {}", tags.join(", "))}],
                "structuredContent": {"projects": tags}
            })
        }
        Err(e) => error_result(format!("listProjects failed: {e}")),
    }
}

fn who_am_i_tool(auth: &AuthContext) -> Value {
    let payload = json!({
        "userId": auth.user.id,
        "email": auth.user.email,
        "name": auth.user.name,
        "sessionId": auth.key_id,
    });
    json!({
        "content": [{"type": "text", "text": payload.to_string()}],
        "structuredContent": payload
    })
}

async fn memory_graph_tool(
    state: &McpState,
    auth: &AuthContext,
    header_tag: Option<&str>,
    args: &Value,
) -> Value {
    let tag = match effective_tag(state, auth, args, header_tag) {
        Ok(tag) => tag,
        Err(error) => return error_result(error),
    };
    // Phase 1: summarize the profile as a stand-in for the full graph feed.
    let preq = ProfileRequest {
        container_tag: tag.clone(),
        q: None,
        threshold: None,
        filters: None,
        include: None,
        buckets: None,
    };
    let (summary, count) = match state.engine.profile(&auth.org.id, &preq).await {
        Ok(p) => {
            let n = p.profile.dynamic.as_ref().map(|d| d.len()).unwrap_or(0);
            (format::profile_section(&p.profile), n)
        }
        Err(_) => ("_(no data)_".to_string(), 0),
    };
    json!({
        "content": [{"type": "text", "text": format!("Memory graph for {tag}\n\n{summary}")}],
        "structuredContent": {"containerTag": tag, "documents": [], "totalCount": count},
        "_meta": {"ui": {"resourceUri": "ui://memory-graph/mcp-app.html", "visibility": ["app"]}}
    })
}
