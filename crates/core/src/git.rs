/// An opaque commit identifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Oid(git2::Oid);

impl Oid {
    pub fn short(&self) -> String {
        self.0.to_string()[..7].to_string()
    }
}

impl From<git2::Oid> for Oid {
    fn from(oid: git2::Oid) -> Self {
        Self(oid)
    }
}

impl std::fmt::Display for Oid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
