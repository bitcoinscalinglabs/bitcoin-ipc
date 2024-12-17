use actix_web;
use actix_web_httpauth::extractors::bearer::BearerAuth;
use actix_web_httpauth::middleware::HttpAuthentication;

use bitcoin_ipc::provider::rpc;
use bitcoin_ipc::{bitcoin_utils, utils};
use serde_json::json;
use thiserror::Error;

use std::sync::Arc;

use bitcoincore_rpc::{Client, RpcApi};

fn make_bitcoincore_rpc() -> Arc<Client> {
    let (rpc_user, rpc_pass, rpc_url, wallet_name) = match utils::load_env() {
        Ok(env) => env,
        Err(e) => {
            panic!("Error: {}", e);
        }
    };

    let rpc = match bitcoin_utils::init_rpc_client(rpc_user, rpc_pass, rpc_url) {
        Ok(rpc) => rpc,
        Err(e) => {
            panic!("Error: {}", e);
        }
    };
    let _ = rpc.load_wallet(&wallet_name);
    let rpc = Arc::new(rpc);
    rpc
}

#[derive(Debug, Error)]
#[error("Unauthorized: Invalid token")]
struct UnauthorizedError;

impl actix_web::error::ResponseError for UnauthorizedError {
    fn error_response(&self) -> actix_web::HttpResponse {
        let json_rpc_error = json!({
            "jsonrpc": "2.0",
            "error": {
                "code": -32601,
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

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    // Init the bitcoincore_rpc client

    let btc_rpc = make_bitcoincore_rpc();

    // Load the provider config

    let config = match utils::load_config() {
        Ok(config) => config,
        Err(e) => {
            eprintln!("Couldn't load provider config: {}", e);
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "Couldn't load provider config",
            ));
        }
    };

    // Construct the JSON-RPC server

    let server_data = Arc::new(rpc::ServerData { btc_rpc, config });
    let rpc_server = rpc::make_rpc_server(server_data);

    // Load auth token from env

    let token = std::env::var("PROVIDER_AUTH_TOKEN").map_err(|e| {
        eprintln!("Couldn't load PROVIDER_AUTH_TOKEN: {}", e);
        std::io::Error::new(
            std::io::ErrorKind::Other,
            "Couldn't load PROVIDER_AUTH_TOKEN",
        )
    })?;

    // Start up the actix-web server

    let port = std::env::var("PROVIDER_PORT").unwrap_or_else(|_| "3030".to_string());
    let addr = format!("127.0.0.1:{}", port);
    println!("Server is running on http://127.0.0.1:{}", port);
    println!("Provider available at http://127.0.0.1:{}/api", port);

    actix_web::HttpServer::new(move || {
        let rpc = rpc_server.clone();
        let token = token.clone();
        let auth = HttpAuthentication::with_fn(move |req, credentials| {
            let token = token.clone();
            async move { auth_guard(req, credentials, token).await }
        });

        actix_web::App::new().wrap(auth).service(
            actix_web::web::service("/api")
                .guard(actix_web::guard::Post())
                .finish(rpc.into_web_service()),
        )
    })
    .bind(addr)?
    .run()
    .await
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
                    "code": -32601,
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
