use anyhow::{bail, Context, Result};
use porgous_core::{app, auth, config::Config, crypto, db::Db, scheduler, state::AppState, updater};
use std::path::PathBuf;

fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,tower_http=warn"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
}

fn parse_config_arg(args: &[String]) -> Option<PathBuf> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--config" || arg == "-c" {
            return iter.next().map(PathBuf::from);
        }
        if let Some(value) = arg.strip_prefix("--config=") {
            return Some(PathBuf::from(value));
        }
    }
    None
}

fn positional(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut skip = false;
    for arg in args {
        if skip {
            skip = false;
            continue;
        }
        if arg == "--config" || arg == "-c" {
            skip = true;
            continue;
        }
        if arg.starts_with("--config=") || arg.starts_with('-') {
            continue;
        }
        out.push(arg.clone());
    }
    out
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();
    let args: Vec<String> = std::env::args().collect();
    let command = args.get(1).map(|s| s.as_str()).unwrap_or("serve");
    let config_path = parse_config_arg(&args);
    match command {
        "--version" | "-V" | "version" => {
            println!("porgous {}", porgous_core::VERSION);
            return Ok(());
        }
        "help" | "--help" | "-h" => {
            print_help();
            return Ok(());
        }
        "serve" => {
            let config = Config::load(config_path)?;
            let state = AppState::bootstrap(config).await?;
            scheduler::repair_schedules(&state).await;
            scheduler::spawn(state.clone());
            app::serve(state).await?;
        }
        "migrate" => {
            let config = Config::load(config_path)?;
            let db = Db::connect(&config.database_url).await?;
            println!("database is up to date ({:?})", db.backend);
        }
        "admin" => {
            let rest = positional(&args[2..]);
            let action = rest.first().map(|s| s.as_str()).unwrap_or("help");
            match action {
                "create" => {
                    let email = flag_value(&args, "--email")
                        .or_else(|| rest.get(1).cloned())
                        .context(
                        "usage: porgous admin create --email <email> --username <name> [--password <pass>]",
                    )?;
                    let username = flag_value(&args, "--username").unwrap_or_else(|| "admin".into());
                    let password = flag_value(&args, "--password")
                        .unwrap_or_else(|| format!("{}{}", crypto::random_token(18), "aA1!"));
                    let config = Config::load(config_path)?;
                    let state = AppState::bootstrap(config).await?;
                    let user = auth::create_user(&state.db, &email, &username, &password, true)
                        .await
                        .map_err(|e| anyhow::anyhow!("{e}"))?;
                    println!("created administrator {} <{}>", user.username, user.email);
                    println!("password: {password}");
                    println!("Store this password now; it is not shown again.");
                }
                "reset-password" => {
                    let email = flag_value(&args, "--email")
                        .or_else(|| rest.get(1).cloned())
                        .context("usage: porgous admin reset-password --email <email>")?;
                    let config = Config::load(config_path)?;
                    let state = AppState::bootstrap(config).await?;
                    let user = auth::find_user_by_email(&state.db, &email)
                        .await
                        .map_err(|e| anyhow::anyhow!("{e}"))?
                        .context("user not found")?;
                    let password = format!("{}{}", crypto::random_token(18), "aA1!");
                    let hash = crypto::hash_password(&password)?;
                    sqlx::query(&state.db.sql("UPDATE users SET password_hash = ?, totp_enabled = 0, totp_secret = NULL WHERE id = ?"))
                        .bind(hash)
                        .bind(&user.id)
                        .execute(&state.db.pool)
                        .await?;
                    println!("new password for {}: {password}", user.email);
                }
                _ => print_help(),
            }
        }
        "update" => {
            let config = Config::load(config_path)?;
            let state = AppState::bootstrap(config).await?;
            if args.iter().any(|a| a == "--check") {
                let status = updater::status(&state).await;
                println!("{}", serde_json::to_string_pretty(&status)?);
            } else if args.iter().any(|a| a == "--rollback") {
                let result = updater::rollback(&state)
                    .await
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                let restart = args.iter().any(|a| a == "--restart");
                let result = updater::apply_latest(&state, restart)
                    .await
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
                println!("{}", serde_json::to_string_pretty(&result)?);
            }
        }
        "keygen" => {
            let out = positional(&args[1..])
                .first()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("porgous-signing.key"));
            use base64::Engine;
            let seed: [u8; 32] = crypto::random_bytes(32).try_into().unwrap();
            let encoded = base64::engine::general_purpose::STANDARD.encode(seed);
            std::fs::write(&out, &encoded)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&out, std::fs::Permissions::from_mode(0o600))?;
            }
            println!("secret key written to {}", out.display());
            println!(
                "public key (put in updates.public_key): {}",
                crypto::ed25519_public_from_seed(&seed)
            );
        }
        "sign" => {
            let rest = positional(&args[1..]);
            let key_path = rest
                .first()
                .context("usage: porgous sign <secret-key-file> <file>")?;
            let file_path = rest
                .get(1)
                .context("usage: porgous sign <secret-key-file> <file>")?;
            use base64::Engine;
            let raw = std::fs::read_to_string(key_path)?;
            let bytes = base64::engine::general_purpose::STANDARD.decode(raw.trim())?;
            let seed: [u8; 32] = bytes
                .try_into()
                .map_err(|_| anyhow::anyhow!("secret key must be 32 bytes"))?;
            let data = std::fs::read(file_path)?;
            let signature = crypto::ed25519_sign(&seed, &data);
            let sig_path = format!("{file_path}.sig");
            std::fs::write(
                &sig_path,
                base64::engine::general_purpose::STANDARD.encode(signature),
            )?;
            println!("signature written to {sig_path}");
        }
        other => {
            bail!("unknown command '{other}'. Run 'porgous help'.");
        }
    }
    Ok(())
}

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == flag {
            return iter.next().cloned();
        }
        if let Some(value) = arg.strip_prefix(&format!("{flag}=")) {
            return Some(value.to_string());
        }
    }
    None
}

fn print_help() {
    println!(
        "\
Porgous {} - lightweight game server management

USAGE:
  porgous [serve] [--config <file>]     Start the panel (default)
  porgous migrate [--config <file>]     Apply database migrations
  porgous admin create --email <email> --username <name> [--password <pass>]
  porgous admin reset-password --email <email>
  porgous update --check                Check GitHub releases
  porgous update [--restart]            Verify and install the latest release
  porgous update --rollback             Restore the previous binary
  porgous keygen [secret-file]          Generate an Ed25519 release signing key
  porgous sign <secret-file> <file>     Sign SHA256SUMS for a release
  porgous version                       Print the version

Environment overrides: PORGOUS_DATA_DIR, PORGOUS_DATABASE_URL, PORGOUS_BIND,
PORGOUS_PUBLIC_URL, PORGOUS_SECRET_KEY, PORGOUS_ADMIN_EMAIL,
PORGOUS_ADMIN_PASSWORD, PORGOUS_TLS_CERT, PORGOUS_TLS_KEY and others.
",
        porgous_core::VERSION
    );
}
