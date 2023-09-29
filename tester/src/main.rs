mod backup;
mod bootstrap;

use std::{path::PathBuf, time::Duration};

use binprot::BinProtRead;
use structopt::StructOpt;
use reqwest::{Url, blocking::Client};

#[derive(StructOpt)]
struct Args {
    #[structopt(long)]
    url: Url,
    #[structopt(subcommand)]
    command: Command,
}

#[derive(StructOpt)]
enum Command {
    Backup {
        #[structopt(long)]
        path: PathBuf,
    },
    Apply,
}

fn main() {
    env_logger::init();

    let Args { url, command } = Args::from_args();

    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();

    let (root, head) = serde_json::from_reader::<_, (u32, u32)>(
        client.get(url.join("root").unwrap()).send().unwrap(),
    )
    .unwrap();

    let ledger_bytes = client
        .get(url.join("ledger").unwrap())
        .send()
        .unwrap()
        .bytes()
        .unwrap();

    let blocks = (root..head).map(|level| {
        let url = url
            .join("transitions/")
            .unwrap()
            .join(&level.to_string())
            .unwrap();
        client.get(url).send().unwrap()
    });

    match command {
        Command::Backup { path } => backup::run(path, &ledger_bytes, blocks),
        Command::Apply => {
            let mut s = ledger_bytes.as_ref();
            let (ledger, aux) = BinProtRead::binprot_read(&mut s).unwrap();
            bootstrap::again(
                ledger,
                aux,
                blocks.map(|mut reader| BinProtRead::binprot_read(&mut reader).unwrap()),
            );
        }
    }
}
