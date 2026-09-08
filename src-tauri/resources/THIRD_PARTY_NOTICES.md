# Intern third-party notices

Intern 0.1.0-alpha.6 is (c) 2026 Vistage Worldwide, Inc. and is distributed
under the Elastic License 2.0; see `LICENSE` in the source repository. This
notice covers the third-party software shipped in, linked into, or downloaded
by the Windows package. `runtime-assets.json` is the authoritative,
SHA-256-addressed inventory of native files bundled by the release build. `Cargo.lock` and
`package-lock.json` are the authoritative source-package inventories.

Model weights are not part of the installer or GitHub release. When the user
chooses local model setup, Intern downloads the separately licensed model files
described by `model-manifest.json` directly to the user's local application data.

## Native runtime assets

| Component | Pinned source | License | Copyright / project |
| --- | --- | --- | --- |
| llama.cpp | `b10361`, Windows CPU x64 archive | MIT | llama.cpp contributors |
| PDFium binaries | `chromium/7881`, Windows x64 | BSD-3-Clause | The PDFium Authors / Chromium Authors |
| Tesseract OCR | 5.5.2, built with vcpkg baseline `644588ca32576d86325fb3fe3b6020042bee61b8` | Apache-2.0 | Tesseract OCR contributors |
| tessdata_fast English and OSD data | commit `87416418657359cb625c412a48b6e1d6d41c29bd` | Apache-2.0 | Tesseract OCR contributors |
| vcpkg | baseline `644588ca32576d86325fb3fe3b6020042bee61b8` | MIT | Microsoft Corporation and contributors |
| Leptonica | version resolved by the pinned vcpkg baseline | BSD-2-Clause | Dan Bloomberg and contributors |

Sources:

- <https://github.com/ggml-org/llama.cpp>
- <https://pdfium.googlesource.com/pdfium/>
- <https://github.com/bblanchon/pdfium-binaries>
- <https://github.com/tesseract-ocr/tesseract>
- <https://github.com/tesseract-ocr/tessdata_fast>
- <https://github.com/microsoft/vcpkg>
- <https://github.com/DanBloomberg/leptonica>

The pinned vcpkg build can include runtime DLLs from Tesseract's dependency
closure. Depending on the pinned port graph, those files can include libraries
from libarchive, libjpeg-turbo, libpng, libtiff, zlib, zstd, WebP, OpenJPEG,
GIFLIB, libxml2, liblzma, bzip2, Brotli, and ICU. Their exact file names and
digests are recorded in `runtime-assets.json`; their upstream notices and source
links are:

| Library family | License family | Source |
| --- | --- | --- |
| libarchive | BSD-2-Clause | <https://github.com/libarchive/libarchive> |
| libjpeg-turbo | BSD-3-Clause, IJG, and Zlib | <https://github.com/libjpeg-turbo/libjpeg-turbo> |
| libpng | libpng-2.0 | <https://github.com/pnggroup/libpng> |
| libtiff | libtiff | <https://gitlab.com/libtiff/libtiff> |
| zlib | Zlib | <https://github.com/madler/zlib> |
| zstd | BSD-3-Clause and GPL-2.0-only for optional programs not bundled here | <https://github.com/facebook/zstd> |
| libwebp | BSD-3-Clause | <https://chromium.googlesource.com/webm/libwebp> |
| OpenJPEG | BSD-2-Clause | <https://github.com/uclouvain/openjpeg> |
| GIFLIB | MIT | <https://sourceforge.net/projects/giflib/> |
| libxml2 | MIT | <https://gitlab.gnome.org/GNOME/libxml2> |
| XZ Utils / liblzma | 0BSD for current releases; see upstream file notices | <https://github.com/tukaani-project/xz> |
| bzip2 | bzip2-1.0.6 | <https://sourceware.org/bzip2/> |
| Brotli | MIT | <https://github.com/google/brotli> |
| ICU | Unicode-3.0 | <https://github.com/unicode-org/icu> |

Only runtime executables, DLLs, and trained-data files needed by Intern are
copied. Headers, static libraries, debug binaries, package-manager tools, and
unrelated command-line programs are not included.

## Application frameworks and libraries

The shipped application includes code from these direct dependency families;
transitive versions are pinned in the lockfiles.

| Component | License |
| --- | --- |
| Tauri and Tauri plugins | Apache-2.0 OR MIT |
| React and React DOM | MIT |
| Lucide icons | ISC |
| AnyDoc | see the pinned crate package metadata and source distribution |
| serde / serde_json | Apache-2.0 OR MIT |
| image | Apache-2.0 OR MIT |
| pdfium-render | Apache-2.0 OR MIT |
| thiserror | Apache-2.0 OR MIT |
| zip | MIT |
| base64 | Apache-2.0 OR MIT |

Build- and test-only tools such as Vite, Vitest, TypeScript, Playwright, and
Testing Library do not ship as separately executable runtime assets, but their
exact versions and license metadata remain recorded in `package-lock.json`.

## License texts

### MIT License

Permission is hereby granted, free of charge, to any person obtaining a copy of
this software and associated documentation files (the "Software"), to deal in
the Software without restriction, including without limitation the rights to
use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of
the Software, and to permit persons to whom the Software is furnished to do so,
subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.

### BSD 2-Clause License

Redistribution and use in source and binary forms, with or without modification,
are permitted provided that the following conditions are met:

1. Redistributions of source code must retain the above copyright notice, this
   list of conditions and the following disclaimer.
2. Redistributions in binary form must reproduce the above copyright notice,
   this list of conditions and the following disclaimer in the documentation
   and/or other materials provided with the distribution.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND
ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED
WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE FOR
ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES
(INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES;
LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON
ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
(INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF THIS
SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

### BSD 3-Clause License

Redistribution and use in source and binary forms, with or without modification,
are permitted provided that the following conditions are met:

1. Redistributions of source code must retain the above copyright notice, this
   list of conditions and the following disclaimer.
2. Redistributions in binary form must reproduce the above copyright notice,
   this list of conditions and the following disclaimer in the documentation
   and/or other materials provided with the distribution.
3. Neither the name of the copyright holder nor the names of its contributors
   may be used to endorse or promote products derived from this software without
   specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND
ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED
WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE FOR
ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES
(INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES;
LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON
ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
(INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF THIS
SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

The complete Apache License 2.0 text is available at
<https://www.apache.org/licenses/LICENSE-2.0>. Component-specific copyright
statements and additional license texts are retained in each pinned upstream
source distribution and vcpkg package copyright file.
