use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    let (api_id, api_hash) = match arguments() {
        Ok(values) => values,
        Err(message) => {
            eprintln!("{message}");
            eprintln!(
                "Использование: cargo run -p telegram-mcp-core --example qr-spike -- <api_id> <api_hash>"
            );
            return ExitCode::from(2);
        }
    };

    match telegram_mcp_core::qr_login::run(api_id, &api_hash).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Вход не выполнен: {error}");
            ExitCode::from(1)
        }
    }
}

fn arguments() -> Result<(i32, String), &'static str> {
    let mut args = std::env::args().skip(1);
    let api_id = args.next().ok_or("нужны api_id и api_hash")?;
    let api_hash = args.next().ok_or("нужны api_id и api_hash")?;
    if args.next().is_some() {
        return Err("нужны только api_id и api_hash");
    }
    let api_id = api_id
        .parse()
        .map_err(|_| "api_id должен быть целым числом")?;
    if api_hash.is_empty() {
        return Err("api_hash пустой");
    }
    Ok((api_id, api_hash))
}
