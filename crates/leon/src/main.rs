#[cfg(feature = "cli")]
fn main() -> miette::Result<()> {
    use std::{collections::HashMap, error::Error, io::stdout};

    use clap::Parser;
    use leon::Template;

    /// Render a Leon template with the given values.
    #[derive(Parser, Debug)]
    #[command(author, version, about, long_about = None)]
    struct Args {
        /// Leon template
        template: String,

        /// Default to use for missing keys
        #[arg(long)]
        default: Option<String>,

        /// Use values from the environment
        #[arg(long)]
        env: bool,

        /// Key-value pairs to use
        #[arg(short, long, value_parser = parse_key_val::<String, String>)]
        values: Vec<(String, String)>,
    }

    /// Parse a single key-value pair
    fn parse_key_val<T, U>(s: &str) -> Result<(T, U), Box<dyn Error + Send + Sync + 'static>>
    where
        T: std::str::FromStr,
        T::Err: Error + Send + Sync + 'static,
        U: std::str::FromStr,
        U::Err: Error + Send + Sync + 'static,
    {
        let (k, v) = s
            .split_once('=')
            .ok_or_else(|| format!("invalid KEY=value: no `=` found in `{s}`"))?;
        Ok((k.parse()?, v.parse()?))
    }

    let args = Args::parse();
    let mut values: HashMap<String, String> = HashMap::from_iter(args.values);
    if args.env {
        for (key, value) in std::env::vars() {
            values.entry(key).or_insert(value);
        }
    }

    let template = args.template;
    let mut template = Template::parse(&template)?;
    if let Some(default) = &args.default {
        template.set_default(default);
    }

    template.render_into(&mut stdout().lock(), &values)?;
    Ok(())
}

#[cfg(not(feature = "cli"))]
fn main() {}
