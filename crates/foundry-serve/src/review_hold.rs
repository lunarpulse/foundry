use crate::{identity::Capability, ServeState};
use axum::{Json, extract::{FromRequestParts, Path, State}, http::{StatusCode, header, request::Parts}, response::IntoResponse};
use std::sync::Arc;

pub struct ReviewBearer(Option<String>);
impl<S:Send+Sync> FromRequestParts<S> for ReviewBearer {
    type Rejection=std::convert::Infallible;
    async fn from_request_parts(parts:&mut Parts,_:&S)->Result<Self,Self::Rejection>{
        Ok(Self(parts.headers.get(header::AUTHORIZATION).and_then(|v|v.to_str().ok()).and_then(|v|v.strip_prefix("Bearer ")).map(str::to_owned)))
    }
}
#[derive(serde::Deserialize)] pub struct ResumeBody { pub target:String }

pub async fn resume(State(st):State<Arc<ServeState>>,auth:ReviewBearer,Path((id,hold)):Path<(String,String)>,Json(body):Json<ResumeBody>)->impl IntoResponse {
    resolve(st,auth,id,hold,foundry_exec::ReviewHoldAction::Resume{target:body.target}).await
}
pub async fn commit_pass(State(st):State<Arc<ServeState>>,auth:ReviewBearer,Path((id,hold)):Path<(String,String)>)->impl IntoResponse {
    resolve(st,auth,id,hold,foundry_exec::ReviewHoldAction::CommitPass).await
}
pub async fn commit_fail(State(st):State<Arc<ServeState>>,auth:ReviewBearer,Path((id,hold)):Path<(String,String)>)->impl IntoResponse {
    resolve(st,auth,id,hold,foundry_exec::ReviewHoldAction::CommitFail).await
}
pub async fn abort(State(st):State<Arc<ServeState>>,auth:ReviewBearer,Path((id,hold)):Path<(String,String)>)->impl IntoResponse {
    resolve(st,auth,id,hold,foundry_exec::ReviewHoldAction::Abort).await
}

async fn resolve(st:Arc<ServeState>,auth:ReviewBearer,id:String,hold:String,action:foundry_exec::ReviewHoldAction)->axum::response::Response {
    let Some(token)=auth.0 else{return (StatusCode::UNAUTHORIZED,Json(serde_json::json!({"error":"bearer session required"}))).into_response()};
    let user={let identity=st.identity.lock().await;match identity.resolve_session(&token){Ok(user)=>user,Err(e)=>return (StatusCode::UNAUTHORIZED,Json(serde_json::json!({"error":e.to_string()}))).into_response()}};
    {let identity=st.identity.lock().await;if let Err(e)=identity.authorize(&user,Capability::ResolveReviewHold){return (StatusCode::FORBIDDEN,Json(serde_json::json!({"error":e.to_string()}))).into_response();}}
    match st.line.resolve_review_hold(&id,&hold,action,&user.username).await {
        Ok(state)=>(StatusCode::OK,Json(serde_json::json!({"order_id":id,"state":state.to_string()}))).into_response(),
        Err(e)=>(StatusCode::INTERNAL_SERVER_ERROR,Json(serde_json::json!({"error":e.to_string()}))).into_response(),
    }
}

