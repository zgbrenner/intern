import { readdir, readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

export async function readVcpkgRuntimeMetadata(installRoot, triplet) {
  const paragraphs = (await readFile(join(installRoot, 'vcpkg/status'), 'utf8')).split(/(?:\r?\n){2,}/);
  const packages = new Map();
  for (const paragraph of paragraphs) {
    const fields = Object.fromEntries(paragraph.split(/\r?\n/).flatMap((line) => {
      const match = /^([^:]+):\s*(.+)$/.exec(line);
      return match ? [[match[1], match[2]]] : [];
    }));
    if (fields.Package && fields.Version && fields.Architecture === triplet && fields.Status === 'install ok installed') {
      packages.set(fields.Package, { name: fields.Package, version: fields.Version });
    }
  }
  if (!packages.has('tesseract')) throw new Error('vcpkg status omits installed Tesseract');

  const owners = {};
  const infoRoot = join(installRoot, 'vcpkg/info');
  for (const entry of (await readdir(infoRoot, { withFileTypes: true })).filter((item) => item.isFile() && item.name.endsWith('.list')).sort((a, b) => a.name.localeCompare(b.name))) {
    const candidates = [...packages.values()].filter((item) => entry.name.startsWith(`${item.name}_`) && entry.name.endsWith(`_${triplet}.list`)).sort((a, b) => b.name.length - a.name.length);
    if (candidates.length === 0) throw new Error(`cannot resolve vcpkg owner for ${entry.name}`);
    const owner = candidates[0];
    for (const line of (await readFile(join(infoRoot, entry.name), 'utf8')).split(/\r?\n/)) {
      const path = line.trim().replaceAll('\\', '/').replace(/^\/+/, '');
      if (!path) continue;
      const previous = owners[path];
      if (previous && (previous.name !== owner.name || previous.version !== owner.version)) throw new Error(`conflicting vcpkg owners for ${path}`);
      owners[path] = owner;
    }
  }
  return { triplet, packages: [...packages.values()].sort((a, b) => a.name.localeCompare(b.name)), owners };
}

async function runCli() {
  const argument = (name) => process.argv.find((value) => value.startsWith(`--${name}=`))?.slice(name.length + 3);
  const installRoot = argument('install-root');
  const triplet = argument('triplet');
  const output = argument('output');
  if (!installRoot || !triplet || !output) throw new Error('usage: --install-root=<path> --triplet=<triplet> --output=<json>');
  await writeFile(output, `${JSON.stringify(await readVcpkgRuntimeMetadata(installRoot, triplet), null, 2)}\n`);
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) await runCli();
