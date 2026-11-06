//! Personal API keys live in the macOS Keychain, never in frontend storage.
use anyhow::{ensure, Result};
use serde::Serialize;

const SERVICE: &str = "app.mukure.player.subtitles";
pub fn validate(provider: &str) -> Result<()> {
    ensure!(
        matches!(provider, "subdl" | "wyzie"),
        "Proveedor desconocido"
    );
    Ok(())
}

#[cfg(target_os = "macos")]
pub fn key(provider: &str) -> Result<Option<String>> {
    validate(provider)?;
    match security_framework::passwords::get_generic_password(SERVICE, provider) {
        Ok(bytes) => Ok(Some(String::from_utf8(bytes)?)),
        Err(error) if error.code() == -25300 => Ok(None),
        Err(_) => anyhow::bail!("No se pudo acceder al Llavero de macOS"),
    }
}
#[cfg(not(target_os = "macos"))]
pub fn key(provider: &str) -> Result<Option<String>> {
    validate(provider)?;
    Ok(None)
}

#[cfg(target_os = "macos")]
pub fn save(provider: &str, key: &str) -> Result<()> {
    validate(provider)?;
    ensure!(
        !key.trim().is_empty() && key.len() <= 1024 && !key.chars().any(char::is_control),
        "Clave inválida"
    );
    security_framework::passwords::set_generic_password(SERVICE, provider, key.trim().as_bytes())
        .map_err(|_| anyhow::anyhow!("No se pudo guardar la clave en el Llavero"))
}
#[cfg(not(target_os = "macos"))]
pub fn save(_: &str, _: &str) -> Result<()> {
    anyhow::bail!("Las conexiones guardadas requieren macOS en esta versión")
}

#[cfg(target_os = "macos")]
pub fn remove(provider: &str) -> Result<()> {
    validate(provider)?;
    match security_framework::passwords::delete_generic_password(SERVICE, provider) {
        Ok(()) => Ok(()),
        Err(error) if error.code() == -25300 => Ok(()),
        Err(_) => anyhow::bail!("No se pudo eliminar la clave del Llavero"),
    }
}
#[cfg(not(target_os = "macos"))]
pub fn remove(provider: &str) -> Result<()> {
    validate(provider)
}

#[derive(Serialize)]
pub struct ProviderStatus {
    pub id: String,
    pub connected: bool,
}

#[tauri::command]
pub async fn get_subtitle_providers() -> Result<Vec<ProviderStatus>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        ["subdl", "wyzie"]
            .into_iter()
            .map(|id| {
                key(id).map(|key| ProviderStatus {
                    id: id.into(),
                    connected: key.is_some(),
                })
            })
            .collect::<Result<Vec<_>>>()
    })
    .await
    .map_err(|_| "No se pudo consultar el Llavero".to_string())?
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn connect_subtitle_provider(
    provider: String,
    api_key: Option<String>,
) -> Result<(), String> {
    validate(&provider).map_err(|e| e.to_string())?;
    let key = if let Some(value) = api_key {
        value.trim().to_owned()
    } else {
        let id = provider.clone();
        tauri::async_runtime::spawn_blocking(move || key(&id))
            .await
            .map_err(|_| "No se pudo consultar el Llavero")?
            .map_err(|e| e.to_string())?
            .ok_or("Primero ingresá una clave")?
    };
    if key.is_empty() || key.len() > 1024 || key.chars().any(char::is_control) {
        return Err("Clave inválida".into());
    }
    // Test before storing. A bad replacement never overwrites a working key.
    crate::online_subtitles::test_key(&provider, &key)
        .await
        .map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn_blocking(move || save(&provider, &key))
        .await
        .map_err(|_| "No se pudo guardar la conexión")?
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn disconnect_subtitle_provider(provider: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || remove(&provider))
        .await
        .map_err(|_| "No se pudo desconectar")?
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn open_subtitle_provider_page(provider: String, app: tauri::AppHandle) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    let url = match provider.as_str() {
        "subdl" => "https://subdl.com/panel/api",
        "wyzie" => "https://store.wyzie.io/redeem",
        "subdl-docs" => "https://subdl.com/api-doc/",
        "wyzie-docs" => "https://docs.wyzie.io/subs/usage/api-keys",
        _ => return Err("Página de proveedor desconocida".into()),
    };
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|_| "No se pudo abrir el navegador".into())
}
