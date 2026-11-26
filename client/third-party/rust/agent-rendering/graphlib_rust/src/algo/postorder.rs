use crate::algo::dfs::dfs;
use crate::Graph;
use std::fmt::Debug;

// TODO: need to check if exceptions are required
pub fn postorder<GL: Default, N: Default + Clone + Debug, E: Default + Clone + Debug>(
    g: &mut Graph<GL, N, E>,
    vs: &Vec<String>,
) -> Vec<String> {
    return match dfs(g, vs, "post") {
        Ok(t) => t,
        _ => vec![],
    };
}
