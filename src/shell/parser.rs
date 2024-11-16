use anyhow::Result;

pub struct Cmd<'a> {
    pub name: &'a str,
    pub args: Vec<&'a str>,
}

pub fn parse_cmd(line: &str) -> Result<Vec<Cmd>> {
    line.split('|')
        .map(|part| {
            let mut parts = part.trim().split_whitespace();
            let name = parts.next().unwrap_or_default();
            let args = parts.collect();
            Ok(Cmd { name, args })
        })
        .collect()
}
