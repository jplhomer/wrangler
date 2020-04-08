use std::path::Path;

use reqwest::blocking::Client;
use serde::Deserialize;

use crate::commands::kv::bucket::{sync, upload_files, AssetManifest};
use crate::commands::kv::bulk::delete::delete_bulk;
use crate::commands::publish;
use crate::http;
use crate::settings::global_user::GlobalUser;
use crate::settings::toml::Target;
use crate::terminal::message;
use crate::upload;

use console::style;

#[derive(Debug, Deserialize)]
struct Preview {
    id: String,
}

impl From<ApiPreview> for Preview {
    fn from(api_preview: ApiPreview) -> Preview {
        Preview {
            id: api_preview.preview_id,
        }
    }
}

// When making authenticated preview requests, we go through the v4 Workers API rather than
// hitting the preview service directly, so its response is formatted like a v4 API response.
// These structs are here to convert from this format into the Preview defined above.
#[derive(Debug, Deserialize)]
struct ApiPreview {
    pub preview_id: String,
}

#[derive(Debug, Deserialize)]
struct V4ApiResponse {
    pub result: ApiPreview,
}

const SITES_UNAUTH_PREVIEW_ERR: &str =
    "Unauthenticated preview does not work for previewing Workers Sites; you need to \
     authenticate to upload your site contents.";

// Builds and uploads the script and its bindings. Returns the ID of the uploaded script.
pub fn upload(
    target: &mut Target,
    user: Option<&GlobalUser>,
    sites_preview: bool,
    verbose: bool,
) -> Result<String, failure::Error> {
    let preview = match &user {
        Some(user) => {
            log::info!("GlobalUser set, running with authentication");

            let missing_fields = validate(&target);

            if missing_fields.is_empty() {
                let client = http::legacy_auth_client(&user);

                if let Some(site_config) = target.site.clone() {
                    let site_namespace = publish::add_site_namespace(user, target, true)?;

                    let path = Path::new(&site_config.bucket);
                    let (to_upload, to_delete, asset_manifest) =
                        sync(target, user, &site_namespace.id, path)?;

                    // First, upload all existing files in given directory
                    if verbose {
                        message::info("Uploading updated files...");
                    }

                    upload_files(target, user, &site_namespace.id, to_upload)?;

                    let preview = authenticated_upload(&client, &target, Some(asset_manifest))?;
                    if !to_delete.is_empty() {
                        if verbose {
                            message::info("Deleting stale files...");
                        }

                        delete_bulk(target, user, &site_namespace.id, to_delete)?;
                    }

                    preview
                } else {
                    authenticated_upload(&client, &target, None)?
                }
            } else {
                message::warn(&format!(
                    "Your wrangler.toml is missing the following fields: {:?}",
                    missing_fields
                ));
                message::warn("Falling back to unauthenticated preview.");
                if sites_preview {
                    failure::bail!(SITES_UNAUTH_PREVIEW_ERR)
                }

                unauthenticated_upload(&target)?
            }
        }
        None => {
            let wrangler_config_msg = style("`wrangler config`").yellow().bold();
            let docs_url_msg = style("https://developers.cloudflare.com/workers/tooling/wrangler/configuration/#using-environment-variables").blue().bold();
            message::billboard(
                &format!("You have not provided your Cloudflare credentials.\n\nPlease run {} or visit\n{}\nfor info on authenticating with environment variables.", wrangler_config_msg, docs_url_msg)
            );

            message::info("Running preview without authentication.");

            if sites_preview {
                failure::bail!(SITES_UNAUTH_PREVIEW_ERR)
            }

            unauthenticated_upload(&target)?
        }
    };

    Ok(preview.id)
}

fn validate(target: &Target) -> Vec<&str> {
    let mut missing_fields = Vec::new();

    if target.account_id.is_empty() {
        missing_fields.push("account_id")
    };
    if target.name.is_empty() {
        missing_fields.push("name")
    };

    match &target.kv_namespaces {
        Some(kv_namespaces) => {
            for kv in kv_namespaces {
                if kv.binding.is_empty() {
                    missing_fields.push("kv-namespace binding")
                }

                if kv.id.is_empty() {
                    missing_fields.push("kv-namespace id")
                }
            }
        }
        None => {}
    }

    missing_fields
}

fn authenticated_upload(
    client: &Client,
    target: &Target,
    asset_manifest: Option<AssetManifest>,
) -> Result<Preview, failure::Error> {
    let create_address = format!(
        "https://api.cloudflare.com/client/v4/accounts/{}/workers/scripts/{}/preview",
        target.account_id, target.name
    );
    log::info!("address: {}", create_address);

    let script_upload_form = upload::form::build(target, asset_manifest, None)?;

    let res = client
        .post(&create_address)
        .multipart(script_upload_form)
        .send()?
        .error_for_status()?;

    let text = &res.text()?;
    log::info!("Response from preview: {:#?}", text);

    let response: V4ApiResponse =
        serde_json::from_str(text).expect("could not create a script on cloudflareworkers.com");

    Ok(Preview::from(response.result))
}

fn unauthenticated_upload(target: &Target) -> Result<Preview, failure::Error> {
    let create_address = "https://cloudflareworkers.com/script";
    log::info!("address: {}", create_address);

    // KV namespaces are not supported by the preview service unless you authenticate
    // so we omit them and provide the user with a little guidance. We don't error out, though,
    // because there are valid workarounds for this for testing purposes.
    let script_upload_form = if target.kv_namespaces.is_some() {
        message::warn(
            "KV Namespaces are not supported in preview without setting API credentials and account_id",
        );
        let mut target = target.clone();
        target.kv_namespaces = None;
        upload::form::build(&target, None, None)?
    } else {
        upload::form::build(&target, None, None)?
    };
    let client = http::client();
    let res = client
        .post(create_address)
        .multipart(script_upload_form)
        .send()?
        .error_for_status()?;

    let text = &res.text()?;
    log::info!("Response from preview: {:#?}", text);

    let preview: Preview =
        serde_json::from_str(text).expect("could not create a script on cloudflareworkers.com");

    Ok(preview)
}
