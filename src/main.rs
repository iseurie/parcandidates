#![feature(conservative_impl_trait, universal_impl_trait)]
extern crate select;
extern crate tokio_core;
extern crate futures;
extern crate regex;
extern crate hyper;
extern crate reqwest;
extern crate num_cpus;
extern crate chan;

use futures::{future, Future, Stream};
use regex::bytes::Regex;
use hyper::{Uri, Client};
use hyper::client::Connect;

fn scrape_ips(haystack: &[u8]) -> Vec<[u8; 4]> {
    let needle = Regex::new(r"\b(?:(?:25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9]?[0-9])\.){3}(?:25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9]?[0-9])\b").expect("compile regex");
    needle.find_iter(haystack).map(|ipv4| {
        let mut octets = [0u8; 4];
        let octetstrs = ipv4.as_bytes()
                            .split(|b| *b == '.' as u8)
                            .enumerate();
        for (ostri, octetstr) in octetstrs {
            let mut octet = 0u8;
            for (i, dbyte) in octetstr.iter().rev().enumerate() {
                octet += (dbyte - '0' as u8) * 10u8.pow(i as u32);
            }
            octets[ostri] = octet;
        }
        octets
    }).collect()
}

fn trawl_net_pg<C: Connect>(http: &Client<C>, uri: Uri) -> 
        impl Future<Item=impl Stream<Item=[u8; 4], Error=hyper::Error>, Error=hyper::Error> {
    use futures::stream::iter_ok;
    // create future of body
    http.get(uri)
        .and_then(|r| future::ok(r.body()))
        .map(|b| {
            b.map(|chunk| scrape_ips(chunk.as_ref()))
             .map(|v| v.into_iter()).map(iter_ok)
             .flatten()
        })
}

fn main() {
    use std::io::{self, Write};
    use std::str::FromStr;
    use select::document::Document;
    use tokio_core::reactor::Core;
    use std::time::Instant;
    use std::thread;

    let stdout = io::stdout();
    let networks = reqwest::get("http://irc.netsplit.de/networks/")
        .expect("retrieve IRC listing")
        .text()
        .expect("IRC listing: Extract text payload");
    let doc = Document::from(networks.as_str());
    use select::predicate::{Predicate, Class, Name};
    let netw_uri_p = Class("competitor").and(Name("a"));
    // queue URIs into a channel and close it (lexical scope / Drop)
    let wg = chan::WaitGroup::new();
    let (uric, rxuri) = {
        let (tx, rx) = chan::async();
        let c = doc.find(netw_uri_p)
                   .filter_map(|n| n.attr("href"))
                   .map(|s| String::from("http://irc.netsplit.de") + s)
                   .map(|s| tx.send(s))
                   .count();
        wg.add(c as i32);
        (c, rx)
    };

    let start_time = Instant::now();
    for _ in 0..num_cpus::get() {
        let wg = wg.clone();
        let rxuri = rxuri.clone();
        let hdl = thread::spawn(move || {
            let rxuri = rxuri.clone();
            let mut lp = Core::new().expect("create thread-local event loop");
            let http = Client::new(&lp.handle());
            let stdout = io::stdout();
            for uristr in rxuri {
                if let Ok(uri) = Uri::from_str(uristr.as_str()) {
                    let reqfut
                    = trawl_net_pg(&http, uri).and_then(|ips| {
                        ips.for_each(|ip| {
                            let mut octets = ip.iter();
                            let mut lck = stdout.lock();
                            for _ in 0..3 {
                                let _ = lck.write(octets.next().unwrap().to_string().as_bytes());
                                let _ = lck.write(b".");
                            }
                            let _ = lck.write(octets.next().unwrap().to_string().as_bytes());
                            let _ = lck.write(b"\n");
                            future::ok(())
                        })
                    });
                    // println!("GET {}...", uristr);
                    let res = lp.run(reqfut);
                    if let Err(what) = res {
                        eprintln!("GET {}: {}", uristr, what);
                    }
                }
                wg.done();
            }
        });
        hdl.join().expect("join child thread");
    }
    let (sec, nsec) = {
        let e = start_time.elapsed();
        (e.as_secs(), e.subsec_nanos())
    };
    println!("{} URIs scraped; {}+{}e-9s elapsed", uric, sec, nsec)
}
