use std::{io::Cursor, path::Path};

use anyhow::Context as _;
use lopdf::{
    content::{Content, Operation},
    dictionary, Document, Object, Stream,
};

use crate::{
    context::Context,
    events::{ExportCbzEvent, ExportPdfEvent},
    pages::{self, PageSource},
    types::{Comic, ComicInfo},
};

enum Archive {
    Cbz,
    Pdf,
}
impl Archive {
    pub fn extension(&self) -> &str {
        match self {
            Archive::Cbz => pages::CBZ_EXTENSION,
            Archive::Pdf => "pdf",
        }
    }
}

struct CbzEventGuard {
    uuid: String,
    context: Context,
    success: bool,
}

impl Drop for CbzEventGuard {
    fn drop(&mut self) {
        if self.success {
            self.context.emit(ExportCbzEvent::End {
                uuid: self.uuid.clone(),
            });
        } else {
            self.context.emit(ExportCbzEvent::Error {
                uuid: self.uuid.clone(),
            });
        }
    }
}
/// Export the cbz archive of `comic` into the configured export directory
///
/// Comics downloaded as a cbz archive are copied, downloads made before the cbz
/// format was introduced are packed into an archive on the fly.
pub fn cbz(context: &Context, comic: &Comic) -> anyhow::Result<()> {
    let comic_title = &comic.title;
    let event_uuid = uuid::Uuid::new_v4().to_string();

    context.emit(ExportCbzEvent::Start {
        uuid: event_uuid.clone(),
        title: comic_title.clone(),
    });
    // Event guard to ensure that the error event is sent if the function panics
    let mut cbz_event_guard = CbzEventGuard {
        uuid: event_uuid.clone(),
        context: context.clone(),
        success: false,
    };

    let download_dir = comic
        .comic_download_dir
        .as_ref()
        .context("`comic_download_dir` field is `None`")?;
    let export_dir = comic
        .get_comic_export_dir(context)
        .context("Failed to get comic export directory")?;
    // Ensure export directory exists
    std::fs::create_dir_all(&export_dir).context(format!(
        "`{comic_title}` failed to create directory `{}`",
        export_dir.display()
    ))?;

    let extension = Archive::Cbz.extension();
    let download_dir_name = &comic
        .get_comic_download_dir_name()
        .context("Failed to get comic download directory name")?;
    let export_cbz_path = export_dir.join(format!("{download_dir_name}.{extension}"));

    let download_cbz_path = pages::cbz_path_of(download_dir).context(format!(
        "`{comic_title}` failed to get the cbz path of `{}`",
        download_dir.display()
    ))?;

    if download_cbz_path.exists() {
        // The comic was downloaded as a cbz archive, exporting it is just a copy
        std::fs::copy(&download_cbz_path, &export_cbz_path).context(format!(
            "`{comic_title}` failed to copy `{}` to `{}`",
            download_cbz_path.display(),
            export_cbz_path.display()
        ))?;
    } else {
        // The comic was downloaded before the cbz format was introduced,
        // so its loose pages have to be packed into an archive
        let comic_info = ComicInfo::from_comic(comic);
        let image_paths = pages::collect_dir_pages(download_dir).context(format!(
            "`{comic_title}` failed to collect the pages of `{}`",
            download_dir.display()
        ))?;

        pages::write_cbz(&export_cbz_path, &comic_info, &image_paths).context(format!(
            "`{comic_title}` failed to write `{}`",
            export_cbz_path.display()
        ))?;
    }
    // Set success to true to ensure that the end event is sent
    cbz_event_guard.success = true;

    Ok(())
}

struct PdfEventGuard {
    uuid: String,
    context: Context,
    success: bool,
}

impl Drop for PdfEventGuard {
    fn drop(&mut self) {
        let uuid = self.uuid.clone();

        if self.success {
            self.context.emit(ExportPdfEvent::End { uuid });
        } else {
            self.context.emit(ExportPdfEvent::Error { uuid });
        }
    }
}

pub fn pdf(context: &Context, comic: &Comic) -> anyhow::Result<()> {
    let comic_title = &comic.title;
    let event_uuid = uuid::Uuid::new_v4().to_string();

    context.emit(ExportPdfEvent::Start {
        uuid: event_uuid.clone(),
        title: comic_title.clone(),
    });

    // Event guard to ensure that the error event is sent if the function panics
    let mut pdf_event_guard = PdfEventGuard {
        uuid: event_uuid.clone(),
        context: context.clone(),
        success: false,
    };

    let download_dir = comic
        .comic_download_dir
        .as_ref()
        .context("`comic_download_dir` field is `None`")?;
    let export_dir = comic
        .get_comic_export_dir(context)
        .context("Failed to get comic export directory")?;
    // Ensure export directory exists
    std::fs::create_dir_all(&export_dir).context(format!(
        "Failed to create directory `{}`",
        export_dir.display()
    ))?;
    // Create PDF
    let extension = Archive::Pdf.extension();
    let download_dir_name = &comic
        .get_comic_download_dir_name()
        .context("Failed to get comic download directory name")?;
    let pdf_path = export_dir.join(format!("{download_dir_name}.{extension}"));
    write_pdf(download_dir, &pdf_path)?;
    // Set success to true to ensure that the end event is sent
    pdf_event_guard.success = true;

    Ok(())
}

/// Write the PDF of the comic downloaded to `download_dir` to `pdf_path`
///
/// Unlike [`pdf`] this needs no export directory and sends no events, the web version
/// uses it to hand a PDF to a browser.
pub fn write_pdf(download_dir: &Path, pdf_path: &Path) -> anyhow::Result<()> {
    let page_source = PageSource::open(download_dir).context(format!(
        "Failed to open the pages of `{}`",
        download_dir.display()
    ))?;
    create_pdf(&page_source, pdf_path).context("Failed to create PDF")
}

/// Create a PDF with the pages of `page_source` and save it to `pdf_path`
#[allow(clippy::similar_names)]
#[allow(clippy::cast_possible_truncation)]
fn create_pdf(page_source: &PageSource, pdf_path: &Path) -> anyhow::Result<()> {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let mut page_ids = vec![];

    for index in 0..page_source.page_count() {
        let page_data = page_source
            .read_page(index)
            .context(format!("Failed to read page `{index}`"))?;
        let (width, height) = image_dimensions(&page_data)
            .context(format!("Failed to get dimensions of page `{index}`"))?;
        let image_stream = lopdf::xobject::image_from(page_data)
            .context(format!("Failed to create image stream for page `{index}`"))?;
        // Add image stream to doc
        let img_id = doc.add_object(image_stream);
        // Image name for the Do operation to display the image on the page
        let img_name = format!("X{}", img_id.0);
        // Used to set image position and size on the page
        let cm_operation = Operation::new(
            "cm",
            vec![
                width.into(),
                0.into(),
                0.into(),
                height.into(),
                0.into(),
                0.into(),
            ],
        );
        // Used to display the image
        let do_operation = Operation::new("Do", vec![Object::Name(img_name.as_bytes().to_vec())]);
        // Create a page, set the image position and size, and then display the image
        // Since we're creating a PDF from scratch, there's no need to use q and Q operations to save and restore graphics state
        let content = Content {
            operations: vec![cm_operation, do_operation],
        };
        let content_id = doc.add_object(Stream::new(dictionary! {}, content.encode()?));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
            "MediaBox" => vec![0.into(), 0.into(), width.into(), height.into()],
        });
        // Add the image as XObject to the document
        // The Do operation can only reference XObject (that's why we defined the Do operation with img_name as parameter, not img_id)
        doc.add_xobject(page_id, img_name.as_bytes(), img_id)?;
        // Record the ID of the newly created page
        page_ids.push(page_id);
    }
    // Add "Pages" to the doc
    let pages_dict = dictionary! {
        "Type" => "Pages",
        "Count" => page_ids.len() as u32,
        "Kids" => page_ids.into_iter().map(Object::Reference).collect::<Vec<_>>(),
    };
    doc.objects.insert(pages_id, Object::Dictionary(pages_dict));
    // Create a new "Catalog" object, add the "Pages" object to the "Catalog" object, then add the "Catalog" object to doc
    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    doc.trailer.set("Root", catalog_id);

    doc.compress();

    doc.save(pdf_path)
        .context(format!("Failed to save `{}`", pdf_path.display()))?;
    Ok(())
}

/// Read the dimensions of the image in `image_data`
///
/// Only the header of the image is read, it is never decoded.
fn image_dimensions(image_data: &[u8]) -> anyhow::Result<(u32, u32)> {
    let dimensions = image::ImageReader::new(Cursor::new(image_data))
        .with_guessed_format()
        .context("Failed to guess the image format")?
        .into_dimensions()
        .context("Failed to read the image dimensions")?;
    Ok(dimensions)
}
