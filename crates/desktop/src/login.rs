//! Signing in to this PC with a card's PIV identity ("login"), behind one
//! platform-neutral face. [`SUPPORTED`] is the single switch: the UI, the
//! card checklist and the phone (through `Accepted.piv_login`) all follow
//! it. Only macOS implements it today (`sc_auth`, see `mac_login`); to add
//! Windows or Linux, implement [`pair_present_card`] and
//! [`present_card_paired`] there and flip
//! `tapbridge_core::wire::platform::piv_login`.

use std::time::Duration;

/// This build can pair a card with the OS login.
pub const SUPPORTED: bool = tapbridge_core::wire::platform::piv_login();

/// What this computer is called in the login wording.
pub const OS_NAME: &str = if cfg!(target_os = "macos") {
    "Mac"
} else if cfg!(windows) {
    "Windows"
} else {
    "computer"
};

/// What setting up the login involves, shown before it starts.
pub const PAIR_HINT: &str = if cfg!(target_os = "macos") {
    "Pairs the card with your account. macOS asks for your password."
} else {
    "Pairs the card with your account."
};

/// Pairs the logged-in user with the PIV identity of the card that is on
/// the phone now. Blocks (runs the OS's own prompts); call off the UI
/// thread. Returns (ok, one sentence for the user).
pub fn pair_present_card() -> (bool, String) {
    #[cfg(target_os = "macos")]
    {
        use crate::mac_login;
        let outcome = match mac_login::current_user() {
            Some(user) => mac_login::pair_present_card(
                &mac_login::SystemRunner,
                &user,
                6,
                Duration::from_secs(1),
            ),
            None => mac_login::Outcome::Failed("unknown user".into()),
        };
        (outcome.ok(), outcome.message())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = Duration::ZERO;
        (
            false,
            format!("Card login isn't available on {OS_NAME} yet."),
        )
    }
}

/// Removes the pairing between the logged-in user and the card that is on
/// the phone now (other cards stay paired). Blocks; call off the UI
/// thread. Returns (ok, one sentence for the user).
pub fn unpair_present_card() -> (bool, String) {
    #[cfg(target_os = "macos")]
    {
        use crate::mac_login;
        let outcome = match mac_login::current_user() {
            Some(user) => mac_login::unpair_present_card(
                &mac_login::SystemRunner,
                &user,
                6,
                Duration::from_secs(1),
            ),
            None => mac_login::UnpairOutcome::Failed("unknown user".into()),
        };
        (outcome.ok(), outcome.message())
    }
    #[cfg(not(target_os = "macos"))]
    {
        (
            false,
            format!("Card login isn't available on {OS_NAME} yet."),
        )
    }
}

/// Whether the card on the phone is already paired with the login, read
/// without changing anything. `None` when that can't be told (yet).
/// Blocks; call off the UI thread.
pub fn present_card_paired() -> Option<bool> {
    #[cfg(target_os = "macos")]
    {
        use crate::mac_login;
        mac_login::present_card_paired(&mac_login::SystemRunner, 4, Duration::from_secs(1))
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}
