import { createHash } from 'node:crypto';
import { deflateSync } from 'node:zlib';
import { mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const FIXED_DOS_TIME = 0;
const FIXED_DOS_DATE = 0x21;
const FIXTURE_DIRECTORY = dirname(fileURLToPath(import.meta.url));

const GOLD = {
  schema_version: 1,
  generated_at: '2000-01-01T00:00:00.000Z',
  notice: 'All names, organizations, addresses, account numbers, and events are fictional.',
  fixtures: [
    { file: 'employment-agreement.pdf', kind: 'text_pdf', document_date: '2025-02-14', document_type: 'Employment Agreement', subject: 'Mira Vale', parties: ['Northstar Lantern Works LLC', 'Mira Vale'], expected_readiness: 'ready', ambiguity: [], acceptable_description_facts: ['employment', 'February 14, 2025', 'Northstar Lantern Works LLC', 'Mira Vale'], expected_routing: 'native_text' },
    { file: 'scanned-lease.pdf', kind: 'image_only_pdf', document_date: '2024-09-01', document_type: 'Lease Agreement', subject: '47 Juniper Loop', parties: ['Cedar Finch Properties LLC', 'Orion Glass Studio Inc.'], expected_readiness: 'needs_review', ambiguity: ['image_only', 'ocr_required'], acceptable_description_facts: ['lease', 'September 1, 2024', '47 Juniper Loop'], expected_routing: 'ocr' },
    { file: 'mixed-signature.pdf', kind: 'mixed_pdf', document_date: '2025-01-08', document_type: 'Services Agreement', subject: 'Aurora Catalog Project', parties: ['Lumen Kite Cooperative', 'Solstice Index LLC'], expected_readiness: 'needs_review', ambiguity: ['mixed_native_and_scanned_pages'], acceptable_description_facts: ['services', 'January 8, 2025', 'Aurora Catalog Project'], expected_routing: 'mixed_native_ocr' },
    { file: 'nda.docx', kind: 'docx', document_date: '2025-03-03', document_type: 'Mutual Non-Disclosure Agreement', subject: 'Project Marigold', parties: ['Fable Harbor Labs LLC', 'Copper Wren Design Inc.'], expected_readiness: 'ready', ambiguity: [], acceptable_description_facts: ['non-disclosure', 'March 3, 2025', 'Project Marigold'], expected_routing: 'anydoc' },
    { file: 'multi-date-invoice.pdf', kind: 'text_pdf', document_date: '2025-04-30', document_type: 'Invoice', subject: 'INV-2048', parties: ['Nimbus Orchard Supply Co.', 'Atlas Threadworks LLC'], expected_readiness: 'needs_review', ambiguity: ['invoice_and_due_dates'], acceptable_description_facts: ['invoice', 'INV-2048', 'April 30, 2025', '$1,248.00'], expected_routing: 'native_text' },
    { file: 'meeting-minutes.md', kind: 'markdown', document_date: '2025-05-07', document_type: 'Meeting Minutes', subject: 'Quarterly Operations Review', parties: ['Fictional Meridian Committee'], expected_readiness: 'ready', ambiguity: [], acceptable_description_facts: ['meeting minutes', 'May 7, 2025', 'Quarterly Operations Review'], expected_routing: 'text' },
    { file: 'rotated-low-resolution-scan.png', kind: 'rotated_scan', document_date: '2025-06-12', document_type: 'Delivery Receipt', subject: 'Receipt DR-771', parties: ['Pine Echo Couriers LLC', 'Violet Cartography Studio'], expected_readiness: 'needs_review', ambiguity: ['rotated', 'low_resolution'], acceptable_description_facts: ['delivery receipt', 'DR-771', 'June 12, 2025'], expected_routing: 'ocr' },
    { file: 'encrypted.pdf', kind: 'encrypted_pdf', expected_error: 'PARSE_FAILED', expected_readiness: 'failed', ambiguity: ['password_required'], acceptable_description_facts: [], expected_routing: 'error' },
    { file: 'malformed.pdf', kind: 'malformed_pdf', expected_error: 'PARSE_FAILED', expected_readiness: 'failed', ambiguity: ['malformed_container'], acceptable_description_facts: [], expected_routing: 'error' },
    { file: 'long-document-100-pages.pdf', kind: 'long_pdf', document_date: '2025-07-01', document_type: 'Project Journal', subject: 'Moonlit Archive', expected_readiness: 'ready', ambiguity: [], acceptable_description_facts: ['project journal', 'July 1, 2025', 'Moonlit Archive'], expected_routing: 'native_text' },
    { file: 'document-image.png', kind: 'png', document_date: '2025-07-14', document_type: 'Purchase Order', subject: 'PO-310', expected_readiness: 'needs_review', ambiguity: ['ocr_required'], acceptable_description_facts: ['purchase order', 'PO-310', 'July 14, 2025'], expected_routing: 'ocr' },
    { file: 'document-image.jpg', kind: 'jpeg', document_date: '2025-07-15', document_type: 'Packing Slip', subject: 'PS-311', expected_readiness: 'needs_review', ambiguity: ['ocr_required'], acceptable_description_facts: ['packing slip', 'PS-311', 'July 15, 2025'], expected_routing: 'ocr' },
    { file: 'document-image.tiff', kind: 'tiff', document_date: '2025-07-16', document_type: 'Work Order', subject: 'WO-312', expected_readiness: 'needs_review', ambiguity: ['ocr_required'], acceptable_description_facts: ['work order', 'WO-312', 'July 16, 2025'], expected_routing: 'ocr' },
  ],
};

function u16(value) {
  const bytes = Buffer.alloc(2);
  bytes.writeUInt16LE(value);
  return bytes;
}

function u32(value) {
  const bytes = Buffer.alloc(4);
  bytes.writeUInt32LE(value >>> 0);
  return bytes;
}

function crc32(bytes) {
  let crc = 0xffffffff;
  for (const byte of bytes) {
    crc ^= byte;
    for (let bit = 0; bit < 8; bit += 1) crc = (crc >>> 1) ^ (0xedb88320 & -(crc & 1));
  }
  return (crc ^ 0xffffffff) >>> 0;
}

function zip(entries) {
  const local = [];
  const central = [];
  let offset = 0;
  for (const [name, value] of entries) {
    const nameBytes = Buffer.from(name);
    const bytes = Buffer.isBuffer(value) ? value : Buffer.from(value);
    const checksum = crc32(bytes);
    const header = Buffer.concat([
      Buffer.from('504b0304', 'hex'), u16(20), u16(0), u16(0), u16(FIXED_DOS_TIME), u16(FIXED_DOS_DATE),
      u32(checksum), u32(bytes.length), u32(bytes.length), u16(nameBytes.length), u16(0), nameBytes,
    ]);
    local.push(header, bytes);
    central.push(Buffer.concat([
      Buffer.from('504b0102', 'hex'), u16(20), u16(20), u16(0), u16(0), u16(FIXED_DOS_TIME), u16(FIXED_DOS_DATE),
      u32(checksum), u32(bytes.length), u32(bytes.length), u16(nameBytes.length), u16(0), u16(0), u16(0), u16(0),
      u32(0), u32(offset), nameBytes,
    ]));
    offset += header.length + bytes.length;
  }
  const centralBytes = Buffer.concat(central);
  return Buffer.concat([...local, centralBytes, Buffer.concat([
    Buffer.from('504b0506', 'hex'), u16(0), u16(0), u16(entries.length), u16(entries.length),
    u32(centralBytes.length), u32(offset), u16(0),
  ])]);
}

function pdfEscape(value) {
  return value.replaceAll('\\', '\\\\').replaceAll('(', '\\(').replaceAll(')', '\\)');
}

function pdfStream(dictionary, bytes) {
  const payload = Buffer.isBuffer(bytes) ? bytes : Buffer.from(bytes, 'latin1');
  return Buffer.concat([Buffer.from(`<< ${dictionary} /Length ${payload.length} >>\nstream\n`), payload, Buffer.from('\nendstream')]);
}

function textCommands(lines) {
  return `BT\n/F1 14 Tf\n50 742 Td\n18 TL\n${lines.map((line, index) => `${index ? 'T*\n' : ''}(${pdfEscape(line)}) Tj`).join('\n')}\nET`;
}

function buildPdf(pages, { encrypted = false } = {}) {
  const objects = [];
  const reserve = () => { objects.push(undefined); return objects.length; };
  const set = (id, value) => { objects[id - 1] = Buffer.isBuffer(value) ? value : Buffer.from(value, 'latin1'); };
  const catalogId = reserve();
  const pagesId = reserve();
  const fontId = reserve();
  const pageIds = [];
  for (const page of pages) {
    const pageId = reserve();
    const contentId = reserve();
    const imageId = page.image ? reserve() : undefined;
    pageIds.push(pageId);
    if (page.image) {
      const compressed = deflateSync(page.image.pixels, { level: 9 });
      set(imageId, pdfStream(`/Type /XObject /Subtype /Image /Width ${page.image.width} /Height ${page.image.height} /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /FlateDecode`, compressed));
      set(contentId, pdfStream('', `q\n512 0 0 640 50 75 cm\n/Im0 Do\nQ`));
      set(pageId, `<< /Type /Page /Parent ${pagesId} 0 R /MediaBox [0 0 612 792] /Resources << /XObject << /Im0 ${imageId} 0 R >> >> /Contents ${contentId} 0 R >>`);
    } else {
      set(contentId, pdfStream('', textCommands(page.lines)));
      set(pageId, `<< /Type /Page /Parent ${pagesId} 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 ${fontId} 0 R >> >> /Contents ${contentId} 0 R >>`);
    }
  }
  set(catalogId, `<< /Type /Catalog /Pages ${pagesId} 0 R >>`);
  set(pagesId, `<< /Type /Pages /Kids [${pageIds.map((id) => `${id} 0 R`).join(' ')}] /Count ${pageIds.length} >>`);
  set(fontId, '<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>');
  let encryptionId;
  if (encrypted) {
    encryptionId = reserve();
    set(encryptionId, '<< /Filter /Standard /V 1 /R 2 /Length 40 /O <00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff> /U <ffeeddccbbaa99887766554433221100ffeeddccbbaa99887766554433221100> /P -4 >>');
  }
  const chunks = [Buffer.from('%PDF-1.7\n%\x80\x81\x82\x83\n', 'latin1')];
  const offsets = [0];
  let length = chunks[0].length;
  objects.forEach((object, index) => {
    offsets.push(length);
    const chunk = Buffer.concat([Buffer.from(`${index + 1} 0 obj\n`), object, Buffer.from('\nendobj\n')]);
    chunks.push(chunk);
    length += chunk.length;
  });
  const xref = length;
  const trailer = `trailer\n<< /Size ${objects.length + 1} /Root ${catalogId} 0 R${encryptionId ? ` /Encrypt ${encryptionId} 0 R /ID [<00112233445566778899aabbccddeeff><00112233445566778899aabbccddeeff>]` : ''} >>\nstartxref\n${xref}\n%%EOF\n`;
  chunks.push(Buffer.from(`xref\n0 ${objects.length + 1}\n0000000000 65535 f \n${offsets.slice(1).map((offset) => `${String(offset).padStart(10, '0')} 00000 n \n`).join('')}${trailer}`));
  return Buffer.concat(chunks);
}

function raster(width, height, seed = 7) {
  const pixels = Buffer.alloc(width * height, 255);
  for (let row = 0; row < 10; row += 1) {
    const y = 25 + row * Math.max(8, Math.floor((height - 50) / 11));
    const lineWidth = Math.min(width - 40, 90 + ((row * 37 + seed * 13) % Math.max(91, width - 80)));
    for (let yy = y; yy < Math.min(height, y + 3); yy += 1) {
      for (let x = 20; x < 20 + lineWidth; x += 1) pixels[yy * width + x] = (x + row + seed) % 19 === 0 ? 90 : 25;
    }
  }
  return { width, height, pixels };
}

const FONT = {
  A: '011101000110001111111000110001', B: '11110100011000111110100011000111110',
  C: '01111100001000010000100001000001111', D: '11110100011000110001100011000111110',
  E: '11111100001000011110100001000011111', F: '11111100001000011110100001000010000',
  G: '01111100001000010111100011000101111', H: '10001100011000111111100011000110001',
  I: '11111001000010000100001000010011111', J: '00111000100001000010100101001001100',
  K: '10001100101010011000101001001010001', L: '10000100001000010000100001000011111',
  M: '10001110111010110101100011000110001', N: '10001110011010110011100011000110001',
  O: '01110100011000110001100011000101110', P: '11110100011000111110100001000010000',
  Q: '01110100011000110001101011001001101', R: '11110100011000111110101001001010001',
  S: '01111100001000001110000010000111110', T: '11111001000010000100001000010000100',
  U: '10001100011000110001100011000101110', V: '10001100011000110001100010101000100',
  W: '10001100011000110101101011101110001', X: '10001100010101000100010101000110001',
  Y: '10001100010101000100001000010000100', Z: '11111000010001000100010001000011111',
  0: '01110100011001110101110011000101110', 1: '00100011000010000100001000010001110',
  2: '01110100010000100010001000100011111', 3: '11110000010000101110000010000111110',
  4: '00010001100101010010111110001000010', 5: '11111100001000011110000010000111110',
  6: '01111100001000011110100011000101110', 7: '11111000010001000100010000100001000',
  8: '01110100011000101110100011000101110', 9: '01110100011000101111000010000111110',
  '-': '00000000000000011111000000000000000', '.': '00000000000000000000000000011000110',
  '/': '00001000100001000100010001000010000', ':': '00000001100011000000001100011000000',
};

function rasterText(lines, width, height, scale = 3) {
  const image = { width, height, pixels: Buffer.alloc(width * height, 255) };
  const glyphWidth = 5 * scale;
  const lineHeight = 9 * scale;
  lines.forEach((line, lineIndex) => {
    let x = 12;
    const y = 12 + lineIndex * lineHeight;
    for (const character of line.toUpperCase()) {
      const glyph = FONT[character];
      if (glyph) {
        for (let row = 0; row < 7; row += 1) for (let column = 0; column < 5; column += 1) {
          if (glyph[row * 5 + column] !== '1') continue;
          for (let dy = 0; dy < scale; dy += 1) for (let dx = 0; dx < scale; dx += 1) {
            const pixelX = x + column * scale + dx;
            const pixelY = y + row * scale + dy;
            if (pixelX < width && pixelY < height) image.pixels[pixelY * width + pixelX] = 0;
          }
        }
      }
      x += glyphWidth + scale;
      if (x + glyphWidth >= width) break;
    }
  });
  return image;
}

function rotateClockwise(image) {
  const rotated = { width: image.height, height: image.width, pixels: Buffer.alloc(image.width * image.height, 255) };
  for (let y = 0; y < image.height; y += 1) for (let x = 0; x < image.width; x += 1) {
    rotated.pixels[x * rotated.width + (rotated.width - y - 1)] = image.pixels[y * image.width + x];
  }
  return rotated;
}

function png(image, description) {
  const signature = Buffer.from('89504e470d0a1a0a', 'hex');
  const chunk = (type, data) => {
    const name = Buffer.from(type);
    const payload = Buffer.concat([name, data]);
    return Buffer.concat([Buffer.from([(data.length >>> 24) & 255, (data.length >>> 16) & 255, (data.length >>> 8) & 255, data.length & 255]), payload, Buffer.from([(crc32(payload) >>> 24) & 255, (crc32(payload) >>> 16) & 255, (crc32(payload) >>> 8) & 255, crc32(payload) & 255])]);
  };
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(image.width, 0); ihdr.writeUInt32BE(image.height, 4); ihdr[8] = 8; ihdr[9] = 0;
  const scanlines = [];
  for (let y = 0; y < image.height; y += 1) scanlines.push(Buffer.from([0]), image.pixels.subarray(y * image.width, (y + 1) * image.width));
  return Buffer.concat([signature, chunk('IHDR', ihdr), chunk('tEXt', Buffer.from(`Description\0${description}`)), chunk('IDAT', deflateSync(Buffer.concat(scanlines), { level: 9 })), chunk('IEND', Buffer.alloc(0))]);
}

function jpeg(description) {
  const base = Buffer.from('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ICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIgIiICIiAiIg//2Q==', 'base64');
  const comment = Buffer.from(description);
  const marker = Buffer.alloc(4);
  marker[0] = 0xff; marker[1] = 0xfe; marker.writeUInt16BE(comment.length + 2, 2);
  return Buffer.concat([base.subarray(0, 2), marker, comment, base.subarray(2)]);
}

function tiff(image, description) {
  const descriptionBytes = Buffer.from(`${description}\0`);
  const entries = 10;
  const ifdOffset = 8;
  const dataOffset = ifdOffset + 2 + entries * 12 + 4;
  const descriptionOffset = dataOffset;
  const pixelsOffset = descriptionOffset + descriptionBytes.length;
  const header = Buffer.alloc(dataOffset);
  header.write('II', 0); header.writeUInt16LE(42, 2); header.writeUInt32LE(ifdOffset, 4); header.writeUInt16LE(entries, ifdOffset);
  const tags = [
    [256, 4, 1, image.width], [257, 4, 1, image.height], [258, 3, 1, 8], [259, 3, 1, 1],
    [262, 3, 1, 1], [270, 2, descriptionBytes.length, descriptionOffset], [273, 4, 1, pixelsOffset],
    [277, 3, 1, 1], [278, 4, 1, image.height], [279, 4, 1, image.pixels.length],
  ];
  tags.forEach(([tag, type, count, value], index) => {
    const offset = ifdOffset + 2 + index * 12;
    header.writeUInt16LE(tag, offset); header.writeUInt16LE(type, offset + 2); header.writeUInt32LE(count, offset + 4);
    if (type === 3 && count === 1) header.writeUInt16LE(value, offset + 8); else header.writeUInt32LE(value, offset + 8);
  });
  header.writeUInt32LE(0, ifdOffset + 2 + entries * 12);
  return Buffer.concat([header, descriptionBytes, image.pixels]);
}

function docx() {
  const document = `<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body>
<w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Mutual Non-Disclosure Agreement</w:t></w:r></w:p>
<w:p><w:r><w:t>Effective March 3, 2025 between Fable Harbor Labs LLC and Copper Wren Design Inc.</w:t></w:r></w:p>
<w:p><w:r><w:t>Confidential purpose: Project Marigold.</w:t></w:r><w:r><w:footnoteReference w:id="1"/></w:r></w:p>
<w:tbl><w:tr><w:tc><w:p><w:r><w:t>Disclosing party</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Either party</w:t></w:r></w:p></w:tc></w:tr></w:tbl>
<w:sectPr><w:headerReference w:type="default" r:id="rIdHeader"/><w:footerReference w:type="default" r:id="rIdFooter"/></w:sectPr></w:body></w:document>`;
  return zip([
    ['[Content_Types].xml', `<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/><Override PartName="/word/header1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/><Override PartName="/word/footer1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml"/><Override PartName="/word/footnotes.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml"/></Types>`],
    ['_rels/.rels', `<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>`],
    ['word/_rels/document.xml.rels', `<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdHeader" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/><Relationship Id="rIdFooter" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer1.xml"/><Relationship Id="rIdFootnotes" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes" Target="footnotes.xml"/></Relationships>`],
    ['word/styles.xml', `<?xml version="1.0" encoding="UTF-8"?><w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/><w:outlineLvl w:val="0"/></w:style></w:styles>`],
    ['word/document.xml', document],
    ['word/header1.xml', `<?xml version="1.0" encoding="UTF-8"?><w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:p><w:r><w:t>PROJECT MARIGOLD - CONFIDENTIAL</w:t></w:r></w:p></w:hdr>`],
    ['word/footer1.xml', `<?xml version="1.0" encoding="UTF-8"?><w:ftr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:p><w:r><w:t>Fictional clean-room fixture</w:t></w:r></w:p></w:ftr>`],
    ['word/footnotes.xml', `<?xml version="1.0" encoding="UTF-8"?><w:footnotes xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:footnote w:id="1"><w:p><w:r><w:t>No real confidential information is included.</w:t></w:r></w:p></w:footnote></w:footnotes>`],
  ]);
}

function sha256(bytes) {
  return createHash('sha256').update(bytes).digest('hex');
}

export async function generateFixtures(outputDirectory) {
  const root = resolve(outputDirectory);
  await rm(root, { recursive: true, force: true });
  await mkdir(join(root, 'mixed-batch'), { recursive: true });
  const scannedLease = rasterText(['LEASE AGREEMENT', 'EFFECTIVE SEPTEMBER 1 2024', 'PROPERTY 47 JUNIPER LOOP', 'CEDAR FINCH PROPERTIES LLC', 'ORION GLASS STUDIO INC'], 768, 960, 3);
  const signature = rasterText(['SIGNATURE PAGE', 'LUMEN KITE COOPERATIVE', 'SOLSTICE INDEX LLC', 'JANUARY 8 2025'], 768, 960, 3);
  const purchaseOrder = rasterText(['PURCHASE ORDER PO-310', 'DATE JULY 14 2025', 'EMBER POST MANUFACTURING LLC'], 560, 300, 3);
  const packingSlip = 'Packing Slip PS-311 dated July 15, 2025 for Quartz Meadow Retail LLC';
  const workOrder = rasterText(['WORK ORDER WO-312', 'DATE JULY 16 2025', 'HARBOR COMET REPAIRS LLC'], 560, 300, 3);
  const rotatedRaster = rotateClockwise(rasterText(['DELIVERY RECEIPT DR-771', 'JUNE 12 2025', 'PINE ECHO COURIERS LLC', 'VIOLET CARTOGRAPHY STUDIO'], 320, 180, 1));
  const invoice = buildPdf([{ lines: ['INVOICE INV-2048', 'Invoice date: April 30, 2025', 'Due date: May 30, 2025', 'Nimbus Orchard Supply Co.', 'Bill to Atlas Threadworks LLC', 'Total: $1,248.00'] }]);
  const files = new Map([
    ['employment-agreement.pdf', buildPdf([{ lines: ['EMPLOYMENT AGREEMENT', 'Effective date: February 14, 2025', 'Employer: Northstar Lantern Works LLC', 'Employee: Mira Vale', 'Work location: 18 Lantern Way, Fictional Harbor, WA 98000'] }])],
    ['scanned-lease.pdf', buildPdf([{ image: scannedLease }])],
    ['mixed-signature.pdf', buildPdf([{ lines: ['SERVICES AGREEMENT', 'Dated January 8, 2025', 'Lumen Kite Cooperative and Solstice Index LLC', 'Project: Aurora Catalog Project'] }, { image: signature }])],
    ['nda.docx', docx()],
    ['multi-date-invoice.pdf', invoice],
    ['meeting-minutes.md', Buffer.from('# Quarterly Operations Review\n\n**Date:** May 7, 2025\n\nThe Fictional Meridian Committee reviewed inventory, safety, and the next quarterly plan.\n')],
    ['rotated-low-resolution-scan.png', png(rotatedRaster, 'Delivery Receipt DR-771 dated June 12, 2025; Pine Echo Couriers LLC; Violet Cartography Studio')],
    ['encrypted.pdf', buildPdf([{ lines: ['Protected fictional record'] }], { encrypted: true })],
    ['malformed.pdf', Buffer.from('%PDF-1.7\n1 0 obj\n<< /Type /Catalog /Pages 99 0 R >>\nendobj\ntruncated clean-room fixture\n')],
    ['long-document-100-pages.pdf', buildPdf(Array.from({ length: 100 }, (_, index) => ({ lines: [`MOONLIT ARCHIVE PROJECT JOURNAL - PAGE ${index + 1}`, 'Journal date: July 1, 2025', `Fictional observation ${String(index + 1).padStart(3, '0')}`] })))],
    ['document-image.png', png(purchaseOrder, 'Purchase Order PO-310 dated July 14, 2025 for Ember Post Manufacturing LLC')],
    ['document-image.jpg', jpeg(packingSlip)],
    ['document-image.tiff', tiff(workOrder, 'Work Order WO-312 dated July 16, 2025 for Harbor Comet Repairs LLC')],
    ['mixed-batch/duplicate-invoice-a.pdf', invoice],
    ['mixed-batch/duplicate-invoice-b.pdf', invoice],
    ['mixed-batch/unsupported.csv', Buffer.from('fictional_id,status\nX-001,unsupported\n')],
    ['mixed-batch/~$nda.docx', Buffer.from('temporary lock file; intentionally ignored')],
  ]);
  for (const [relative, bytes] of files) await writeFile(join(root, relative), bytes);
  const manifest = {
    schema_version: 1,
    generator: { node: '24.15.0' },
    files: [...files].map(([file, bytes]) => ({ file, size: bytes.length, sha256: sha256(bytes) })),
  };
  await writeFile(join(root, 'manifest.json'), `${JSON.stringify(manifest, null, 2)}\n`);
  return structuredClone(GOLD);
}

async function runCli() {
  const updateGold = process.argv.includes('--update-gold');
  const outputArgument = process.argv.find((argument) => argument.startsWith('--output='));
  const output = outputArgument ? outputArgument.slice('--output='.length) : join(FIXTURE_DIRECTORY, 'generated');
  const gold = await generateFixtures(output);
  const expectedPath = join(FIXTURE_DIRECTORY, 'expected.json');
  const manifestPath = join(FIXTURE_DIRECTORY, 'manifest.json');
  const generatedManifest = JSON.parse(await readFile(join(output, 'manifest.json'), 'utf8'));
  if (updateGold) {
    if (process.version !== 'v24.15.0') throw new Error(`gold updates require pinned Node v24.15.0, got ${process.version}`);
    await writeFile(expectedPath, `${JSON.stringify(gold, null, 2)}\n`);
    await writeFile(manifestPath, `${JSON.stringify(generatedManifest, null, 2)}\n`);
  }
  const expected = JSON.parse(await readFile(expectedPath, 'utf8'));
  if (JSON.stringify(expected) !== JSON.stringify(gold)) throw new Error('fixtures/expected.json does not match the clean-room gold definition; review and run with --update-gold');
  const expectedManifest = JSON.parse(await readFile(manifestPath, 'utf8'));
  if (JSON.stringify(expectedManifest) !== JSON.stringify(generatedManifest)) throw new Error('fixtures/manifest.json does not match generated bytes; regenerate with pinned Node v24.15.0 and --update-gold');
  process.stdout.write(`Generated ${gold.fixtures.length} deterministic gold fixtures in ${resolve(output)}\n`);
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) await runCli();
