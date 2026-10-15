use rook_tools::{Tool, ToolContext, pdf::ReadPdf};
use serde_json::{Value, json};
use std::io::Write;
fn document(pages: &[&str]) -> Vec<u8> {
    document_with_size(pages, 10.0)
}
fn document_with_size(pages: &[&str], size: f64) -> Vec<u8> {
    let mut objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        String::new(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
    ];
    let mut refs = Vec::new();
    for text in pages {
        let page = objects.len() + 1;
        refs.push(format!("{page} 0 R"));
        objects.push(format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 3 0 R >> >> /Contents {} 0 R >>",page+1));
        let stream = format!("BT /F1 {size} Tf 20 740 Td ({text}) Tj ET");
        objects.push(format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()));
    }
    objects[1] = format!("<< /Type /Pages /Kids [{}] /Count {} >>", refs.join(" "), pages.len());
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, object) in objects.iter().enumerate() {
        offsets.push(bytes.len());
        write!(bytes, "{} 0 obj\n{object}\nendobj\n", i + 1).unwrap();
    }
    let xref = bytes.len();
    write!(bytes, "xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).unwrap();
    for offset in offsets {
        writeln!(bytes, "{offset:010} 00000 n ").unwrap();
    }
    write!(bytes, "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1)
        .unwrap();
    bytes
}
#[tokio::test]
#[ignore = "requires local Poppler helpers"]
async fn actual_selected_pages_have_provenance_and_changed_sources_cannot_continue() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ToolContext::new(dir.path().into());
    let name = "quotes '$() and spaces.pdf";
    let path = dir.path().join(name);
    std::fs::write(&path, document(&["FIRST_PAGE", "SECOND_PAGE", "THIRD_PAGE"])).unwrap();
    let result = ReadPdf.call(&ctx, &json!({"path":name,"pages":"2-3"})).await.unwrap();
    let result: Value = serde_json::from_str(&result.content).unwrap();
    assert_eq!(result["total_pages"], 3);
    assert_eq!(result["pages"][0]["page"], 2);
    assert_eq!(result["pages"][1]["page"], 3);
    assert!(!result.to_string().contains("FIRST_PAGE"));
    assert!(result["pages"][0]["text"].as_str().unwrap().contains("SECOND_PAGE"));
    assert_eq!(result["sha256"].as_str().unwrap().len(), 64);
    std::fs::write(path, document(&["changed"])).unwrap();
    assert!(
        ReadPdf
            .call(&ctx, &json!({"path":name,"cursor":format!("2:0:{}:{}:text",result["sha256"].as_str().unwrap(),result["pages"][0]["text_sha256"].as_str().unwrap())}))
            .await
            .unwrap_err()
            .to_string()
            .contains("source changed")
    );
}
#[tokio::test]
#[ignore = "requires local Poppler helpers"]
async fn pages_without_text_report_the_missing_evidence_and_render_only_when_requested() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ToolContext::new(dir.path().into());
    std::fs::write(dir.path().join("scan.pdf"), document(&[""])).unwrap();
    let reply = ReadPdf.call(&ctx, &json!({"path":"scan.pdf"})).await.unwrap();
    assert!(reply.images.is_empty());
    let value: Value = serde_json::from_str(&reply.content).unwrap();
    assert_eq!(value["pages"][0]["partial"], true);
    assert!(value["pages"][0]["notice"].as_str().unwrap().contains("not proof"));
    let rendered = ReadPdf.call(&ctx, &json!({"path":"scan.pdf","mode":"render"})).await.unwrap();
    assert_eq!(rendered.images.len(), 1);
    assert!(rendered.images[0].width <= 2048 && rendered.images[0].height <= 2048);
}
#[tokio::test]
async fn reached_input_and_page_bounds_refuse_before_any_decoder_is_started() {
    let dir = tempfile::tempdir().unwrap();
    let mut ctx = ToolContext::new(dir.path().into());
    ctx.documents.max_bytes = 1024;
    let bytes = document(&[&"SOURCE".repeat(500)]);
    assert!(bytes.len() > 1024);
    std::fs::write(dir.path().join("large.pdf"), bytes).unwrap();
    assert!(
        ReadPdf
            .call(&ctx, &json!({"path":"large.pdf"}))
            .await
            .unwrap_err()
            .to_string()
            .contains("documents.max_bytes")
    );
    assert!(
        ReadPdf
            .call(&ctx, &json!({"path":"large.pdf","pages":format!("1-{}",ctx.documents.max_pages+1)}))
            .await
            .unwrap_err()
            .to_string()
            .contains("pages per call")
    );
    assert!(ReadPdf.call(&ctx, &json!({"path":"../outside.pdf"})).await.is_err());
}

#[tokio::test]
#[ignore = "requires local Poppler helpers"]
async fn bounded_page_windows_remain_readable_and_cannot_switch_the_extracted_text() {
    let dir = tempfile::tempdir().unwrap();
    let mut ctx = ToolContext::new(dir.path().into());
    ctx.max_output_bytes = 4096;
    let text = "CONTINUED_PDF_TEXT ".repeat(150);
    std::fs::write(dir.path().join("long.pdf"), document_with_size(&[&text], 0.01)).unwrap();
    let mut args = json!({"path":"long.pdf"});
    let mut seen = String::new();
    let mut calls = 0;
    loop {
        let reply = ReadPdf.call(&ctx, &args).await.unwrap();
        assert!(reply.content.len() <= 4096);
        let value: Value = serde_json::from_str(&reply.content).unwrap();
        let page = &value["pages"][0];
        seen.push_str(page["text"].as_str().unwrap());
        calls += 1;
        if page["next_offset"].is_null() {
            break;
        }
        assert!(calls < 100);
        args["cursor"] = page["next_cursor"].clone();
        let mut wrong = args.clone();
        wrong["cursor"] = json!(format!(
            "1:{}:{}:{}:text",
            page["next_offset"],
            value["sha256"].as_str().unwrap(),
            "0".repeat(64)
        ));
        assert!(ReadPdf.call(&ctx, &wrong).await.unwrap_err().to_string().contains("extraction changed"));
    }
    assert!(calls > 1, "fixture must reach the window limit, text bytes {}", seen.len());
    assert!(seen.contains("CONTINUED_PDF_TEXT"));
}

#[tokio::test]
#[ignore = "requires local Poppler and tesseract helpers"]
async fn optional_local_ocr_keeps_an_explicit_partial_notice() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ToolContext::new(dir.path().into());
    std::fs::write(dir.path().join("scan.pdf"), document(&[""])).unwrap();
    let reply = ReadPdf.call(&ctx, &json!({"path":"scan.pdf","mode":"ocr"})).await.unwrap();
    let value: Value = serde_json::from_str(&reply.content).unwrap();
    assert_eq!(value["pages"][0]["method"], "ocr");
    assert_eq!(value["pages"][0]["partial"], true);
    assert!(value["pages"][0]["notice"].as_str().unwrap().contains("approximate"));
    assert!(reply.images.is_empty());
}
