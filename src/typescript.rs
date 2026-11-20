use std::{
    env,
    path::{Path, PathBuf},
};

pub(crate) fn compiler(root: &Path) -> PathBuf {
    root.join(env::var_os("JSNIR_TSC").unwrap_or_else(|| "node_modules/.bin/tsc".into()))
}
