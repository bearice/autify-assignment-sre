use std::{collections::HashMap, path::PathBuf};

use anyhow::{anyhow, Result};
use clap::{Arg, Command};
use futures::{stream::FuturesUnordered, StreamExt};
use reqwest::{Response, Url};
use tl::{parse, ParserOptions};
use tokio::{fs::File, io::AsyncWriteExt};
use tracing::{error, info, warn, Level};

struct Task {
    url: Url,
    out_name: PathBuf,
}

fn filename_for_url(url: &Url) -> String {
    let path = PathBuf::from(url.path());
    if path.file_name().is_none() {
        format!("{}.html", url.host_str().unwrap())
    } else {
        format!(
            "{}{}",
            url.host_str().unwrap(),
            url.path().replace("/", "_")
        )
    }
}

impl Task {
    fn new(url: Url) -> Self {
        let out_name = filename_for_url(&url).into();
        Self { url, out_name }
    }

    async fn filter_noop(&self, resp: Response) -> Result<Vec<u8>> {
        Ok(resp.bytes().await?.to_vec())
    }

    async fn filter_html(
        &self,
        resp: Response,
        rewrite_assets: bool,
    ) -> Result<(Vec<u8>, Vec<Url>)> {
        // Ensure we are getting an html document
        if resp
            .headers()
            .get("content-type")
            .map_or(true, |ct| !ct.as_bytes().starts_with(b"text/html"))
        {
            warn!("skipping non-html document");
            Ok((resp.bytes().await?.to_vec(), vec![]))
        } else {
            let body = resp.text().await?;
            let mut dom = parse(body.as_str(), ParserOptions::default())?;
            let mut counts = HashMap::new();
            let mut assets = vec![];

            // Just loop on every nodes, we don't care about the hierarchy
            for n in dom.nodes_mut() {
                if let Some(t) = n.as_tag_mut() {
                    let tag = t.name().as_utf8_str().as_ref().to_owned();
                    *counts.entry(tag.clone()).or_insert(0) += 1;
                    // only img tags get rewritten as time is limited, should add other tags (script, link, etc)
                    if rewrite_assets && tag == "img" {
                        self.rewrite_image(t, &mut assets)?;
                    }
                };
            }
            eprintln!(
                "site: {site}\nnum_links: {links}\nimages: {images}\nlast_fetch: {time}",
                site = self.url.domain().unwrap(),
                links = counts.get("a").unwrap_or(&0),
                images = counts.get("img").unwrap_or(&0),
                time = chrono::Local::now().to_rfc2822(),
            );
            let body = if rewrite_assets {
                dom.inner_html()
            } else {
                drop(dom); // has to drop here as it 'borrows' the body
                body
            };
            Ok((body.into(), assets))
        }
    }

    fn rewrite_image(&self, t: &mut tl::HTMLTag, assets: &mut Vec<Url>) -> Result<()> {
        info!("Rewriting image {:?}", t);
        let attrs = t.attributes_mut();
        if let Some(t) = attrs.get_mut("src").flatten() {
            let base_url = Url::options().base_url(Some(&self.url));
            let src = t.as_utf8_str().as_ref().to_owned();
            let url = base_url.parse(&src).unwrap();
            let dst = filename_for_url(&url);
            info!("rewriting asset: {} => {}", src, dst);
            t.set(dst)?;
            assets.push(url);
        }
        Ok(())
    }

    async fn exec(self, show_metadata: bool, rewrite_assets: bool) -> Result<Vec<Task>> {
        info!("Fetching {} => {:?}", self.url, self.out_name);
        let resp = reqwest::get(self.url.clone()).await?;
        if !resp.status().is_success() {
            return Err(anyhow!(
                "Error while fetching {} : code {:?}",
                self.url,
                resp.status()
            ));
        }
        let (body, assets) = if show_metadata {
            self.filter_html(resp, rewrite_assets).await?
        } else {
            (self.filter_noop(resp).await?, vec![])
        };
        let mut out_file = File::create(&self.out_name).await?;
        out_file.write_all(&body).await?;
        Ok(assets.into_iter().map(Task::new).collect())
    }
}

#[tokio::main]
async fn main() {
    let args = Command::new(env!("CARGO_BIN_NAME"))
        .version(env!("CARGO_PKG_VERSION"))
        .trailing_var_arg(true)
        .arg(
            Arg::new("show_metadata")
                .short('m')
                .long("metadata")
                .help("show metadata (section 2)"),
        )
        .arg(
            Arg::new("rewrite_assets")
                .short('r')
                .long("rewrite")
                .help("download and rewrite assets (section 3)"),
        )
        .arg(
            Arg::new("verbose")
                .short('v')
                .long("verbose")
                .help("add more verbosity")
                .max_occurrences(3),
        )
        .arg(Arg::new("urls").multiple_values(true))
        .get_matches();

    let show_metadata = args.is_present("show_metadata");
    let rewrite_assets = args.is_present("rewrite_assets");
    let verbose = args.occurrences_of("verbose") as usize;
    let verbose = match verbose {
        0 => Level::ERROR,
        1 => Level::INFO,
        2 => Level::DEBUG,
        _ => Level::TRACE,
    };
    tracing_subscriber::fmt::fmt()
        .with_max_level(verbose)
        .init();

    let urls: Vec<_> = args.values_of("urls").unwrap_or_default().collect();
    if urls.is_empty() {
        eprintln!("No urls provided");
        return;
    }
    let mut tasks = vec![];
    for url in urls {
        tasks.push(Task::new(Url::parse(url).expect("invalid url")));
    }
    let mut futures = FuturesUnordered::new();
    for task in tasks {
        futures.push(task.exec(show_metadata, rewrite_assets));
    }
    while let Some(res) = futures.next().await {
        match res {
            Ok(sub_tasks) => {
                for task in sub_tasks {
                    futures.push(task.exec(false, false));
                }
            }
            Err(e) => error!("{}", e),
        }
    }
}
