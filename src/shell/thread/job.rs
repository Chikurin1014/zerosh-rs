use std::{
    fmt::{self, Display, Formatter},
    str::FromStr,
};

use nix::unistd;

#[derive(Debug, Clone)]
pub struct JobInfo {
    pub pgid: unistd::Pid,
    pub command: String, // Command
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct JobId(usize);

impl Display for JobId {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for JobId {
    type Err = std::num::ParseIntError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let id = s.parse()?;
        Ok(JobId(id))
    }
}

impl Into<usize> for JobId {
    fn into(self) -> usize {
        self.0
    }
}

impl From<usize> for JobId {
    fn from(id: usize) -> Self {
        JobId(id)
    }
}
