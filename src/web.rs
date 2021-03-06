use std::io;
use std::fs;
use std::path::{Path, PathBuf};
use std::cell::Cell;
use tempfile::Builder;

extern crate image;
use image::io::Reader as ImageReader;

extern crate soup;
use soup::prelude::*;

extern crate url;
use url::{Url, ParseError};

extern crate epub_builder;
use epub_builder::{EpubBuilder, ZipLibrary, EpubContent, ReferenceType, TocElement};

extern crate serde;
extern crate serde_json;
use serde::{Deserialize};

use crate::cmd::{ReadabiliPyCmd, ReadabiliPyParser};

mod errors {
    error_chain! {
         foreign_links {
             Io(std::io::Error);
             HttpRequest(reqwest::Error);
             EpubBuilding(epub_builder::Error);
             ImageReading(image::ImageError);
         }
    }
}

use errors::*;

#[derive(Copy, Clone)]
enum DLFileType {
    Text,
    Image,
}

struct Downloader {
    path: PathBuf,  // Path where all file are collected;
    file_type: Cell<DLFileType>,  // Mutate type with `.set` and `.get` Cell methods
}

impl Downloader {
    fn new(path: PathBuf, file_type: DLFileType) -> Self {
        Self {
            path,
            file_type: Cell::new(file_type),
        }
    }  // new_for_path

    fn download_from(&self, target: Url) -> Result<String> {
        // Make HTTP request for target file
        let mut response = reqwest::blocking::get(target.as_str())?; // TODO: use non-blocking async

        // Choosing filename
        let filename = response
            .url()
            .path_segments()
            .and_then(|segments| segments.last())
            .and_then(|name| if name.is_empty() { None } else { Some(name) })
            .unwrap_or("tmp.bin");

        println!("file to download: '{:?}'", filename);

        // Locate destination
        let local_abs_path = self.path.join(filename);
        println!("will be located under: '{:?}'", local_abs_path);
        let mut destination = fs::File::create(local_abs_path.clone())?;

        // Copy file in destination
        match self.file_type.get() {
            DLFileType::Text => {
                let html_string = response.text()?;
                io::copy(&mut html_string.as_bytes(), &mut destination)
                    .expect("Failed to copy HTML file to destination");
            },  // if HTML
            DLFileType::Image => {
                io::copy(&mut response, &mut destination)
                    .expect("Failed to copy image to destination");
            }  // else if Image
        }  // match file type

        Ok(local_abs_path.into_os_string().into_string().unwrap())
    }
}

#[derive(Deserialize, Debug, Clone)]
pub struct Article {
    title: Option<String>,  // The article title
    byline: Option<String>,  // Author information
    date: Option<String>,
    content: Option<String>,
    plain_content: Option<String>,  // plain content of the article, preserving the HTML structure
}

impl Article {
    pub fn epub_from_url(target: String) -> Result<()> {
        // Parse target URL
        let target_url = Url::parse(&target);

        // Check target URL validity
        match target_url {
            Ok(url) => { println!("{}", url) },
            Err(e) => {
                println!("Error {}, return.", e);
                return Ok(())  // TODO: Implement Error InvalidURL
            }
        };

        // Make temp dir
        let tmp_dir = Builder::new().prefix("kindle-pult_").tempdir()?;
        // Persist the tempdir and return PathBuf
        let tmp_dir_path = tmp_dir.into_path();

        // Set up downloader for HTML files
        let downloader = Downloader::new(tmp_dir_path.clone(), DLFileType::Text);
        let target_url = Url::parse(&target);
        let local_abs_path_string = downloader.download_from(target_url.unwrap());

        // Purify HTML
        let purifier = ReadabiliPyCmd::new(ReadabiliPyParser::Mozilla);  // Select parser

        let outfile_path = tmp_dir_path.join("article.json");  // TODO: use fname
        let outfile_path_string = outfile_path.clone().into_os_string().into_string().unwrap();

        // Generate json file with ReadabiliPy
        // TODO: print feedback to GUI
        purifier.json_from_file(local_abs_path_string.unwrap(), outfile_path_string);

        // Read Json, deserialize and print Rust data structure.
        // TODO: print article info to GUI
        let json_file = fs::File::open(outfile_path).expect("file not found");
        let article: Article = serde_json::from_reader(json_file).expect("error reading json");

        // Get absolute image urls
        let image_urls = match article.clone().content {
            Some(content) => {
                let mut urls = Vec::new();
                let soup = Soup::new(&content);

                for img in soup.tag("img").find_all() {
                    let image_url = img.get("src").expect("Couldn't find `src` attribute");

                    // Make sure URL is absolute and add it to urls vector;
                    match Url::parse(&image_url) {
                        Ok(url) => {
                            urls.push(url);
                        },  // Already absolute, send to vector
                        Err(e) => {
                            match e {
                                ParseError::RelativeUrlWithoutBase => {
                                    println!("Relative URL: {}", &image_url);
                                    let target_url = Url::parse(&target);  // Second parsing
                                    let absolute_url = target_url.unwrap().join(&image_url)
                                        .expect("Can't make absolute URL of image");

                                    println!("absolute URL: {}", &absolute_url);
                                    urls.push(absolute_url);
                                },  // Relative URL error
                                _ => {
                                    println!("errore: {}", e);
                                    return Ok(())
                                }  // Unknown error
                            };  // match error
                        }  // if error
                    }  // match url parse
                };

                println!("Image URLS: {:?}", urls);
                urls
            },
            None => {
                Vec::new()
            } // Empty vector
        };

        // Download images
        downloader.file_type.set(DLFileType::Image);
        let mut local_abs_image_paths = Vec::new();

        for url in image_urls {
            let local_abs_path_string = downloader.download_from(url);
            local_abs_image_paths.push(local_abs_path_string);
        }

        // Build epub
        // Create a new EpubBuilder using the zip library
        let mut epub: Vec<u8> = vec!();
        let mut epub_dest = fs::File::create("book.epub")?;  // TODO: use sluggified title

        let epub_title = article.title.unwrap();
        let epub_author = article.byline.unwrap();
        let epub_content = article.content.unwrap();

        let mut builder = EpubBuilder::new(ZipLibrary::new()?)?;
        builder.metadata("author", epub_author)?;
        builder.metadata("title", epub_title.clone())?;

        for img in local_abs_image_paths {
            // Image string path
            let img = img.unwrap();
            // Get filename and extenstion
            let img_path = Path::new(&img);
            let filename = img_path.file_name().unwrap();
            let ext = img_path.extension().unwrap().to_str().unwrap();
            let ext = format!("image/{}", ext);
            // Open image as DynamicImage
            let img_decoded = ImageReader::open(&img)?.decode()?;

            // Image optimization (using Image or Photon?)

            builder.add_resource(filename, img_decoded.as_bytes(), ext)?;
        };

        // Add title page
        builder.add_content(EpubContent::new("title.xhtml", epub_title.clone().as_bytes())
                     .title(epub_title)
                     .reftype(ReferenceType::TitlePage))?;

        // Add Chapter
        builder.add_content(EpubContent::new("article.xhtml", epub_content.as_bytes()))?;

        builder.generate(&mut epub)?;

        io::copy(&mut &epub[..], &mut epub_dest)
            .expect("Failed to copy epub file to destination");

        // Delete the temporary directory ourselves.
        fs::remove_dir_all(tmp_dir_path)?;

        Ok(())
    }
}
