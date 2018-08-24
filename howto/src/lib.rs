extern crate failure;
#[macro_use]
extern crate lazy_static;
extern crate futures;
extern crate hyper;
extern crate hyper_tls;
extern crate scraper;
extern crate slugify;
extern crate tokio;

pub use failure::Error;

use futures::future::ok;
use futures::stream::futures_ordered;
use futures::{Future, Stream};
use hyper::{Body, Client, Request};
use hyper_tls::HttpsConnector;
use scraper::{Html, Selector};
use slugify::slugify;
use std::sync::mpsc::{channel, Receiver};
use std::thread;

#[derive(Debug, Clone)]
pub struct Answer {
    pub link: String,
    pub full_text: String,
    pub instruction: String,
}

pub struct Answers {
    inner: Receiver<Result<Answer, Error>>,
}

impl Iterator for Answers {
    type Item = Result<Answer, Error>;

    fn next(&mut self) -> Option<Result<Answer, Error>> {
        self.inner.recv().ok()
    }
}

fn get(url: &str) -> impl Future<Item = String, Error = Error> {
    let req = Request::get(url)
        .header(
            "User-Agent",
            "Mozilla/5.0 (X11; Ubuntu; Linux x86_64; rv:22.0) Gecko/20100 101 Firefox/22.0",
        ).body(Body::empty())
        .expect("request construction failed");

    let connector = HttpsConnector::new(4).expect("TLS initialization failed");

    let client = Client::builder().build(connector);

    let resp_future = client.request(req);

    resp_future
        .map(|resp| resp.into_body())
        .and_then(|body| {
            body.fold(vec![], |mut acc, chunk| -> Result<_, hyper::Error> {
                acc.extend_from_slice(&chunk);
                Ok(acc)
            })
        }).map_err(Into::<Error>::into)
        .and_then(|v| String::from_utf8(v).map_err(Into::<Error>::into))
}

fn get_stackoverflow_links(query: &str) -> impl Future<Item = Vec<String>, Error = Error> {
    lazy_static! {
        static ref LINK_SELECTOR: Selector = Selector::parse(".r>a").unwrap();
    }

    let url = format!(
        "https://www.google.com/search?q=site:stackoverflow.com%20{}",
        query
    );
    let query = query.to_string();

    get(&url)
        .map(|content| {
            let html = Html::parse_document(&content);

            let links: Vec<_> = html
                .select(&LINK_SELECTOR)
                .filter_map(|e| e.value().attr("href"))
                .map(ToString::to_string)
                .collect();

            links
        }).map_err(move |e| e.context(format!("error in query {}", query)).into())
}

fn get_answer(link: &str) -> impl Future<Item = Option<Answer>, Error = Error> {
    lazy_static! {
        static ref ANSWER_SELECTOR: Selector = Selector::parse(".answer").unwrap();
        static ref TEXT_SELECTOR: Selector = Selector::parse(".post-text>*").unwrap();
        static ref PRE_INSTRUCTION_SELECTOR: Selector = Selector::parse("pre").unwrap();
        static ref CODE_INSTRUCTION_SELECTOR: Selector = Selector::parse("code").unwrap();
    }

    let url = format!("{}?answerstab=votes", link);
    let link = link.to_string();
    let link1 = link.clone();

    get(&url)
        .map(|content| Html::parse_document(&content))
        .map(|html| {
            html.select(&ANSWER_SELECTOR).next().and_then(|answer| {
                answer
                    .select(&PRE_INSTRUCTION_SELECTOR)
                    .next()
                    .or_else(|| answer.select(&CODE_INSTRUCTION_SELECTOR).next())
                    .map(|e| e.text().collect::<Vec<_>>().join(""))
                    .map(|instruction| {
                        let full_text = answer
                            .select(&TEXT_SELECTOR)
                            .flat_map(|e| e.text())
                            .collect::<Vec<_>>()
                            .join("");

                        Answer {
                            link,
                            instruction,
                            full_text,
                        }
                    })
            })
        }).map_err(move |e| e.context(format!("error in link {}", link1)).into())
}

pub fn howto(query: &str) -> Answers {
    let query = slugify!(query, separator = "+");
    let (sender, receiver) = channel::<Result<Answer, Error>>();

    let links_future = get_stackoverflow_links(&query);

    let answers_stream = links_future
        .map(|v| futures_ordered(v.into_iter().map(|link| get_answer(&link))))
        .flatten_stream()
        .filter_map(|o| o);

    let answers_future = answers_stream
        .map_err({
            let sender = sender.clone();
            move |e| sender.send(Err(e)).unwrap()
        }).for_each(move |a| {
            sender.send(Ok(a)).unwrap();
            ok(())
        });

    thread::spawn(move || {
        tokio::run(answers_future);
    });

    Answers { inner: receiver }
}
