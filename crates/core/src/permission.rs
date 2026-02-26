/// Permission handler for agent tool requests.
///
/// Initial implementation: auto-accept all permissions.
/// A future interactive handler can use channels to prompt the user via the TUI.
pub struct PermissionHandler;

impl Default for PermissionHandler {
    fn default() -> Self {
        Self
    }
}

impl PermissionHandler {
    pub fn new() -> Self {
        Self
    }

    /// Check whether a permission request should be granted.
    /// Currently auto-accepts all requests.
    pub fn check(&self, _description: &str) -> bool {
        true
    }
}
