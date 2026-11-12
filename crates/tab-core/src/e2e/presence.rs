//! "Is it really you?" before the vault opens: Touch ID (or the Mac's
//! password) on macOS, Windows Hello on Windows. Elsewhere there's no
//! system prompt to ask, so the device key alone opens it.

/// Asks the person to confirm it's them.
#[async_trait::async_trait]
pub trait Presence: Send + Sync {
    /// `Ok` once confirmed; an error saying why not otherwise.
    async fn confirm(&self, reason: &str) -> anyhow::Result<()>;
}

/// The operating system's own prompt.
pub struct System;

#[async_trait::async_trait]
impl Presence for System {
    async fn confirm(&self, reason: &str) -> anyhow::Result<()> {
        imp::confirm(reason).await
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use block2::RcBlock;
    use objc2::runtime::Bool;
    use objc2_foundation::{NSError, NSString};
    use objc2_local_authentication::{LAContext, LAPolicy};

    pub async fn confirm(reason: &str) -> anyhow::Result<()> {
        let rx = ask(reason)?;
        rx.await
            .map_err(|_| anyhow::anyhow!("the confirmation was dropped"))?
            .map_err(|e| anyhow::anyhow!("{e}"))
    }

    /// Shows the prompt. LocalAuthentication answers on its own queue; the
    /// context rides along in the reply block so it lives until then.
    fn ask(reason: &str) -> anyhow::Result<tokio::sync::oneshot::Receiver<Result<(), String>>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let ctx = unsafe { LAContext::new() };
        // Touch ID when there is one, else the account password.
        let policy = LAPolicy::DeviceOwnerAuthentication;
        if let Err(e) = unsafe { ctx.canEvaluatePolicy_error(policy) } {
            anyhow::bail!(
                "this Mac can't confirm it's you: {}",
                e.localizedDescription()
            );
        }
        let held = std::sync::Mutex::new(Some((tx, ctx.clone())));
        let reply = RcBlock::new(move |ok: Bool, err: *mut NSError| {
            let out = if ok.as_bool() {
                Ok(())
            } else if err.is_null() {
                Err("not confirmed".to_string())
            } else {
                // SAFETY: LocalAuthentication passes a valid NSError when it fails.
                Err(unsafe { &*err }.localizedDescription().to_string())
            };
            if let Some((tx, _ctx)) = held.lock().unwrap().take() {
                let _ = tx.send(out);
            }
        });
        unsafe {
            ctx.evaluatePolicy_localizedReason_reply(policy, &NSString::from_str(reason), &reply)
        };
        Ok(rx)
    }
}

#[cfg(windows)]
mod imp {
    use windows::Security::Credentials::UI::{
        UserConsentVerificationResult, UserConsentVerifier, UserConsentVerifierAvailability,
    };
    use windows::core::HSTRING;

    pub async fn confirm(reason: &str) -> anyhow::Result<()> {
        let reason = HSTRING::from(reason);
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let available = UserConsentVerifier::CheckAvailabilityAsync()?.join()?;
            if available != UserConsentVerifierAvailability::Available {
                anyhow::bail!(
                    "Windows Hello isn't set up on this PC (Settings → Accounts → Sign-in options)"
                );
            }
            match UserConsentVerifier::RequestVerificationAsync(&reason)?.join()? {
                UserConsentVerificationResult::Verified => Ok(()),
                UserConsentVerificationResult::Canceled => anyhow::bail!("not confirmed"),
                other => anyhow::bail!("Windows Hello didn't confirm it's you ({other:?})"),
            }
        })
        .await?
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod imp {
    pub async fn confirm(_reason: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

/// Always says yes, for tests.
#[cfg(test)]
pub struct Yes;

#[cfg(test)]
#[async_trait::async_trait]
impl Presence for Yes {
    async fn confirm(&self, _reason: &str) -> anyhow::Result<()> {
        Ok(())
    }
}
