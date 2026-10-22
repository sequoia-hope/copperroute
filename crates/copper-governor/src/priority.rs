use crate::Error;

/// Never raises priority: a process already niced further by its parent stays where it is.
#[cfg(unix)]
pub fn lower_priority(nice: i32) -> Result<i32, Error> {
    use rustix::process::{getpriority_process, setpriority_process};
    let current = getpriority_process(None).map_err(|e| Error::Priority(e.to_string()))?;
    if current >= nice {
        return Ok(current);
    }
    setpriority_process(None, nice).map_err(|e| Error::Priority(e.to_string()))?;
    Ok(nice)
}

#[cfg(not(unix))]
pub fn lower_priority(nice: i32) -> Result<i32, Error> {
    Ok(nice)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn never_raises_priority() {
        let current = rustix::process::getpriority_process(None).unwrap();
        assert_eq!(
            lower_priority(current.saturating_sub(1).max(0)).unwrap(),
            current
        );
    }
}
