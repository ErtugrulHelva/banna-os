// Bir OTF/TTF fontu, belirtilen boyutta kenar yumuşatmalı (8 bit kaplama)
// glif atlasına çevirir: swift gen_ui_font.swift <font> <px> <çıktı.bin> [karakterler]
//
// Biçim (küçük endian): "BFNT", u16 boyut, i16 ascent, i16 descent, i16 satır boşluğu,
// u16 glif sayısı; her glif 18 bayt: u32 kod noktası, i16 ilerleme*64, i16 sol,
// i16 üst (taban çizgisinden yukarı), u16 genişlik, u16 yükseklik, u32 veri konumu;
// ardından satır satır kaplama baytları.
import CoreGraphics
import CoreText
import Foundation

let args = CommandLine.arguments
guard args.count >= 4, let px = Double(args[2]) else {
    FileHandle.standardError.write("usage: gen_ui_font <font> <px> <out.bin> [chars]\n".data(using: .utf8)!)
    exit(1)
}
let ascii = (32...126).map { Character(UnicodeScalar($0)!) }
let extra = Array("çÇğĞıİöÖşŞüÜâÂîÎûÛ°•…–—€₺“”‘’«»×÷±≤≥→←↑↓✓µ⚡")
let requested: [Character] = args.count >= 5 ? Array(args[4]) : ascii + extra
let chars = Array(Set(requested)).sorted { $0.unicodeScalars.first!.value < $1.unicodeScalars.first!.value }

let url = URL(fileURLWithPath: args[1]) as CFURL
guard let descriptors = CTFontManagerCreateFontDescriptorsFromURL(url) as? [CTFontDescriptor], let descriptor = descriptors.first else {
    FileHandle.standardError.write("cannot load font\n".data(using: .utf8)!)
    exit(1)
}
let font = CTFontCreateWithFontDescriptor(descriptor, CGFloat(px), nil)
let ascent = CTFontGetAscent(font), descent = CTFontGetDescent(font), leading = CTFontGetLeading(font)

var header = Data(), records = Data(), pixels = Data()
func u16(_ v: Int, _ d: inout Data) { var x = UInt16(truncatingIfNeeded: v).littleEndian; d.append(Data(bytes: &x, count: 2)) }
func u32(_ v: Int, _ d: inout Data) { var x = UInt32(truncatingIfNeeded: v).littleEndian; d.append(Data(bytes: &x, count: 4)) }

var count = 0
for ch in chars {
    let scalars = Array(String(ch).utf16)
    var glyphs = [CGGlyph](repeating: 0, count: scalars.count)
    guard CTFontGetGlyphsForCharacters(font, scalars, &glyphs, scalars.count), glyphs[0] != 0 || ch == " " else { continue }
    var glyph = glyphs[0]
    var advance = CGSize.zero
    CTFontGetAdvancesForGlyphs(font, .horizontal, &glyph, &advance, 1)
    var bounds = CGRect.zero
    CTFontGetBoundingRectsForGlyphs(font, .horizontal, &glyph, &bounds, 1)
    let left = Int(floor(bounds.minX)) - 1
    let bottom = Int(floor(bounds.minY)) - 1
    let width = max(0, Int(ceil(bounds.maxX)) - left + 1)
    let height = max(0, Int(ceil(bounds.maxY)) - bottom + 1)
    let offset = pixels.count
    if width > 0 && height > 0 && !bounds.isEmpty {
        var buf = [UInt8](repeating: 0, count: width * height)
        let ctx = CGContext(data: &buf, width: width, height: height, bitsPerComponent: 8, bytesPerRow: width,
                            space: CGColorSpaceCreateDeviceGray(), bitmapInfo: CGImageAlphaInfo.none.rawValue)!
        ctx.setAllowsAntialiasing(true); ctx.setShouldAntialias(true)
        ctx.setAllowsFontSmoothing(false); ctx.setShouldSmoothFonts(false)
        ctx.setAllowsFontSubpixelPositioning(true); ctx.setShouldSubpixelPositionFonts(true)
        ctx.setFillColor(gray: 1, alpha: 1)
        var position = CGPoint(x: CGFloat(-left), y: CGFloat(-bottom))
        CTFontDrawGlyphs(font, &glyph, &position, 1, ctx)
        for v in buf {
            let a = pow(Double(v) / 255.0, 0.85) * 255.0
            pixels.append(UInt8(min(255, max(0, a.rounded()))))
        }
    }
    u32(Int(ch.unicodeScalars.first!.value), &records)
    u16(Int((advance.width * 64).rounded()), &records)
    u16(left, &records)
    u16(bottom + (width > 0 && height > 0 && !bounds.isEmpty ? height : 0), &records)
    u16(bounds.isEmpty ? 0 : width, &records)
    u16(bounds.isEmpty ? 0 : height, &records)
    u32(offset, &records)
    count += 1
}
header.append("BFNT".data(using: .ascii)!)
u16(Int(px), &header)
u16(Int(ascent.rounded()), &header)
u16(Int(descent.rounded()), &header)
u16(Int(leading.rounded()), &header)
u16(count, &header)
try! (header + records + pixels).write(to: URL(fileURLWithPath: args[3]))
print("\(args[3]): \(count) glyphs, \(pixels.count) bytes of coverage")
