//! Apple Vision OCR (next-build-macos item 4, `.lo/macos-control-research.md` 4): the text lines of a window capture
//! become marks when the AX tree is thin (canvas, games, custom UIs). On-device, `.fast` level: 21-32 ms warm on a
//! 3200x2000 image and it read all 60 labels; `.accurate` was 100-114 ms and merged lines (research 4, measured).
//! No new crate: four Vision classes through objc2 `msg_send!`.

use crate::contract::PtRect;

/// One recognized line: its text and its Vision box (x, y, w, h), normalized 0..1, origin bottom-left.
pub type Line = (String, (f64, f64, f64, f64));

/// A Vision box over an image of `img` (global points, origin top-left, y down) -> global points.
/// The pixel size never enters: Retina or not, the box is a fraction of the captured rect.
pub fn to_points(b: (f64, f64, f64, f64), img: PtRect) -> PtRect {
    let (x, y, w, h) = b;
    PtRect { x: img.x + x * img.w, y: img.y + (1.0 - y - h) * img.h, w: w * img.w, h: h * img.h }
}

#[cfg(target_os = "macos")]
#[link(name = "Vision", kind = "framework")]
unsafe extern "C" {}

/// The text lines of a PNG or JPEG. Err = Vision is missing or refused the image.
#[cfg(target_os = "macos")]
pub fn recognize(image: &[u8]) -> anyhow::Result<Vec<Line>> {
    use objc2::rc::{Allocated, Retained};
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2::msg_send;
    use objc2_foundation::{NSArray, NSData, NSDictionary, NSError, NSRect, NSString};

    let class = |n: &std::ffi::CStr| AnyClass::get(n).ok_or_else(|| anyhow::anyhow!("Vision: no {n:?}"));
    let (handler_c, request_c) = (class(c"VNImageRequestHandler")?, class(c"VNRecognizeTextRequest")?);
    let data = NSData::with_bytes(image);
    let opts = NSDictionary::<NSString, AnyObject>::new();
    // SAFETY: the selectors and argument types are Vision's public API (macOS 10.15+); every object is retained.
    unsafe {
        let a: Allocated<AnyObject> = msg_send![handler_c, alloc];
        let handler: Option<Retained<AnyObject>> = msg_send![a, initWithData: &*data, options: &*opts];
        let handler = handler.ok_or_else(|| anyhow::anyhow!("Vision: the image did not load"))?;
        let req: Retained<AnyObject> = msg_send![request_c, new];
        let _: () = msg_send![&req, setRecognitionLevel: 1isize]; // VNRequestTextRecognitionLevelFast
        let _: () = msg_send![&req, setUsesLanguageCorrection: false];
        let langs = NSArray::from_retained_slice(&[NSString::from_str("es-ES"), NSString::from_str("en-US")]);
        let _: () = msg_send![&req, setRecognitionLanguages: &*langs];
        let reqs = NSArray::from_retained_slice(&[req.clone()]);
        let done: Result<(), Retained<NSError>> = msg_send![&handler, performRequests: &*reqs, error: _];
        done.map_err(|e| anyhow::anyhow!("Vision: {}", e.localizedDescription()))?;
        let results: Option<Retained<NSArray<AnyObject>>> = msg_send![&req, results];
        let mut out = vec![];
        for obs in results.iter().flat_map(|r| r.to_vec()) {
            let top: Retained<NSArray<AnyObject>> = msg_send![&obs, topCandidates: 1usize];
            let Some(c) = top.firstObject() else { continue };
            let s: Retained<NSString> = msg_send![&c, string];
            let b: NSRect = msg_send![&obs, boundingBox];
            out.push((s.to_string(), (b.origin.x, b.origin.y, b.size.width, b.size.height)));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vision_boxes_map_to_screen_points() {
        // A 800 x 500 pt window at (100, 50) on a Retina display (1600 x 1000 px): the box fractions are the same.
        let win = PtRect { x: 100.0, y: 50.0, w: 800.0, h: 500.0 };
        // bottom-left origin: a box at the very top of the image has y = 1 - h
        assert_eq!(to_points((0.0, 0.75, 0.5, 0.25), win), PtRect { x: 100.0, y: 50.0, w: 400.0, h: 125.0 });
        assert_eq!(to_points((0.5, 0.0, 0.5, 0.25), win), PtRect { x: 500.0, y: 425.0, w: 400.0, h: 125.0 });
        // a display left of the main one: negative x stays negative
        let left = PtRect { x: -1920.0, y: 0.0, w: 1920.0, h: 1080.0 };
        assert_eq!(to_points((0.25, 0.5, 0.25, 0.25), left), PtRect { x: -1440.0, y: 270.0, w: 480.0, h: 270.0 });
    }

    /// One real Vision run on a synthetic PNG (tests/fixtures/ocr_labels.png: 1600 x 1000 px, Helvetica 36 px, made
    /// with PIL). No screen capture. The label centers in px: Comercial X (178, 135.5), Enviar factura (1011, 615.5).
    #[cfg(target_os = "macos")]
    #[test]
    fn vision_reads_a_synthetic_png() {
        let png = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/ocr_labels.png")).unwrap();
        let t = std::time::Instant::now();
        let lines = recognize(&png).unwrap();
        eprintln!("vision .fast: {} lines in {} ms (cold)", lines.len(), t.elapsed().as_millis());
        let texts: Vec<&str> = lines.iter().map(|l| l.0.as_str()).collect();
        for want in ["Comercial X", "Bandeja de entrada", "Settings", "Enviar factura", "Nuevo mensaje"] {
            assert!(texts.contains(&want), "{want:?} not in {texts:?}");
        }
        // the window is 800 x 500 pt at (100, 50): px / 2 + origin
        let win = PtRect { x: 100.0, y: 50.0, w: 800.0, h: 500.0 };
        let at = |t: &str| {
            let r = to_points(lines.iter().find(|l| l.0 == t).unwrap().1, win);
            (r.x + r.w / 2.0, r.y + r.h / 2.0)
        };
        for (t, (px, py)) in [("Comercial X", (178.0, 135.5)), ("Enviar factura", (1011.0, 615.5))] {
            let (x, y) = at(t);
            let (wx, wy) = (100.0 + px / 2.0, 50.0 + py / 2.0);
            assert!((x - wx).abs() < 6.0 && (y - wy).abs() < 6.0, "{t}: ({x:.1}, {y:.1}) vs ({wx}, {wy})");
        }
    }
}
