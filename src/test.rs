use pdf_oxide::document::PdfDocument;
use pdf_oxide::pipeline::{TextPipeline, TextPipelineConfig, ReadingOrderContext};
use pdf_oxide::pipeline::converters::{MarkdownOutputConverter, OutputConverter};
use pdf_oxide::converters::ConversionOptions;
use std::error::Error;

fn main() -> Result<(), Box<dyn Error>> {
    // Open the target document
    let doc = PdfDocument::open("Lorem Ipsum.pdf")?;
    
    // FIX 1: Add `?` to extract the usize from the Result
    let total_pages = doc.page_count()?; 
    
    // Process up to the first 10 pages
    let max_pages = std::cmp::min(total_pages, 10);
    for page_idx in 0..max_pages {
        // Extract raw styled character spans along with their absolute coordinates
        let spans = doc.extract_spans(page_idx)?;
        
        // Define conversion configuration options
        let mut conversion_opts = ConversionOptions::default();
        conversion_opts.detect_headings = true;
        
        // FIX 2: Removed `conversion_opts.detect_lists = true;` 
        // as it does not exist on ConversionOptions
        
        // Build the text pipeline config
        let pipeline_config = TextPipelineConfig::from_conversion_options(&conversion_opts);
        let pipeline = TextPipeline::with_config(pipeline_config.clone());
        
        // Run the geometric layout reconstruction algorithms
        let context = ReadingOrderContext::new();
        let ordered_spans = pipeline.process(spans, context)?;
        
        // Render the sorted spans to layout-preserving Markdown
        let converter = MarkdownOutputConverter::new();
        let markdown = converter.convert(&ordered_spans, &pipeline_config)?;
        
        println!("=== Page {} ===", page_idx + 1);
        println!("{}", markdown);
    }
    
    Ok(())
}
