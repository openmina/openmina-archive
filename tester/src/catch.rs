use std::time::Duration;

use reqwest::{Url, blocking::Client};

use mina_p2p_messages::{v2, binprot::BinProtRead};

pub fn run(url: Url, hash: String, level: u32) {
    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();

    let mut level = level;
    let mut hash = hash;
    while level > 9307 {
        let text = client
            .get(url.join("append/").unwrap().join(&hash).unwrap())
            .send()
            .unwrap()
            .text()
            .unwrap();
        log::info!("done {level} {hash} {text}");

        std::thread::sleep(Duration::from_secs(5));

        let url = url
            .join("transitions/")
            .unwrap()
            .join(&level.to_string())
            .unwrap();
        let mut rdr = client.get(url).send().unwrap();
        let block = Vec::<v2::MinaBlockBlockStableV2>::binprot_read(&mut rdr).unwrap();

        let prev = block[0]
            .header
            .protocol_state
            .previous_state_hash
            .to_string();
        if prev == hash {
            return;
        } else {
            hash = prev;
        }

        level -= 1;
    }
}
