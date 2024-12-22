use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use thiserror::Error;

use actix_web_httpauth::extractors::bearer::BearerAuth;
use actix_web_httpauth::middleware::HttpAuthentication;

use super::rpc;

#[derive(Serialize, Deserialize)]
pub enum CustomRPCErrorCode {
    Unauthorized = -32001,
}

#[derive(Debug, Error)]
#[error("Unauthorized: Invalid token")]
struct UnauthorizedError;

impl actix_web::error::ResponseError for UnauthorizedError {
    fn error_response(&self) -> actix_web::HttpResponse {
        let json_rpc_error = json!({
            "jsonrpc": "2.0",
            "error": {
                "code": CustomRPCErrorCode::Unauthorized,
                "message": "Unauthorized: Invalid token"
            },
            "id": null
        });
        actix_web::HttpResponse::Unauthorized()
            .content_type("application/json")
            .body(json_rpc_error.to_string())
    }
}

async fn auth_guard(
    req: actix_web::dev::ServiceRequest,
    credentials: Option<BearerAuth>,
    token: String,
) -> Result<actix_web::dev::ServiceRequest, (actix_web::Error, actix_web::dev::ServiceRequest)> {
    let Some(credentials) = credentials else {
        return Err((UnauthorizedError.into(), req));
    };

    let provided_token = credentials.token();
    if provided_token == token {
        Ok(req)
    } else {
        Err((UnauthorizedError.into(), req))
    }
}

pub struct Server {
    pub token: String,
    pub port: String,
    pub server_data: Arc<rpc::ServerData>,
}

impl Server {
    pub fn new(token: String, port: String, server_data: Arc<rpc::ServerData>) -> Self {
        Self {
            token,
            port,
            server_data,
        }
    }

    pub async fn serve(&self) -> std::io::Result<()> {
        let rpc_server = rpc::make_rpc_server(self.server_data.clone());

        let port = &self.port;
        let addr = format!("127.0.0.1:{}", port);
        println!("Server is running on http://127.0.0.1:{}", port);
        println!("Provider available at http://127.0.0.1:{}/api", port);

        let token = self.token.clone();

        actix_web::HttpServer::new(move || {
            let rpc_server = rpc_server.clone();
            let token = token.clone();
            let auth = HttpAuthentication::with_fn(move |req, credentials| {
                let token = token.clone();
                async move { auth_guard(req, credentials, token).await }
            });

            actix_web::App::new().wrap(auth).service(
                actix_web::web::service("/api")
                    .guard(actix_web::guard::Post())
                    .finish(rpc_server.into_web_service()),
            )
        })
        .bind(addr)?
        .run()
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::dev::ServiceRequest;
    use actix_web::http::header::AUTHORIZATION;
    use actix_web::test::TestRequest;
    use actix_web::FromRequest;
    use actix_web_httpauth::extractors::bearer::BearerAuth;

    async fn bearer_token(token: &str) -> BearerAuth {
        let req =
            TestRequest::default().insert_header((AUTHORIZATION, format!("Bearer {}", token)));
        BearerAuth::from_request(&req.to_http_request(), &mut actix_web::dev::Payload::None)
            .await
            .unwrap()
    }

    async fn check_unauthorized_response(
        result: Result<ServiceRequest, (actix_web::Error, ServiceRequest)>,
    ) {
        assert!(result.is_err());

        if let Err((err, _)) = result {
            let response = err.error_response();
            assert_eq!(response.status(), actix_web::http::StatusCode::UNAUTHORIZED);
            let body = response.into_body();
            let body_bytes = actix_web::body::to_bytes(body).await.unwrap();
            let body_str = std::str::from_utf8(&body_bytes).unwrap();
            let expected_body = json!({
                "jsonrpc": "2.0",
                "error": {
                    "code": CustomRPCErrorCode::Unauthorized,
                    "message": "Unauthorized: Invalid token"
                },
                "id": null
            })
            .to_string();
            assert_eq!(body_str, expected_body);
        }
    }

    #[actix_web::test]
    async fn test_auth_guard_valid_token() {
        let token = "auth_token";
        let req = TestRequest::default().to_srv_request();

        let credentials = bearer_token(token).await;

        let result = auth_guard(req, Some(credentials), token.to_string()).await;
        assert!(result.is_ok());
    }

    #[actix_web::test]
    async fn test_auth_guard_invalid_token() {
        let token = "auth_token";
        let req = TestRequest::default().to_srv_request();
        let credentials = bearer_token("invalid token").await;

        let result = auth_guard(req, Some(credentials), token.to_string()).await;
        check_unauthorized_response(result).await;
    }

    #[actix_web::test]
    async fn test_auth_guard_empty_token() {
        let token = "auth_token";
        let req = TestRequest::default().to_srv_request();

        let result = auth_guard(req, None, token.to_string()).await;
        check_unauthorized_response(result).await;
    }
}
