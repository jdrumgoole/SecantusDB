//! Start a server, use it with the official driver, and let it clean up.
//!
//! ```sh
//! cargo run --example quickstart
//! ```

use mongodb::bson::{doc, Document};
use mongodb::sync::Client;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let server = secantus_mdb::Server::start()?;
    println!("SecantusDB {} at {}", secantus_mdb::VERSION, server.uri());

    let client = Client::with_uri_str(server.uri())?;
    let people = client.database("demo").collection::<Document>("people");
    people
        .insert_many([
            doc! {"name": "Ada", "born": 1815},
            doc! {"name": "Alan", "born": 1912},
        ])
        .run()?;
    for person in people.find(doc! {"born": {"$gt": 1900}}).run()? {
        println!("{}", person?);
    }
    Ok(()) // `server` drops here: it stops and removes its temporary store
}
