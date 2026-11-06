import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { createRequire } from 'node:module';
import { existsSync, realpathSync, readdirSync, readFileSync, mkdirSync, rmSync, copyFileSync, writeFileSync } from 'node:fs';
import { basename, dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const output = join(root, 'src-tauri/resources/notices');
const metadata = JSON.parse(execFileSync('cargo', ['metadata', '--locked', '--format-version', '1', '--filter-platform', 'aarch64-apple-darwin', '--manifest-path', join(root, 'src-tauri/Cargo.toml')], { encoding: 'utf8', maxBuffer: 32 * 1024 * 1024 }));
const overrides = JSON.parse(readFileSync(join(root, 'third-party/license-overrides.json'), 'utf8'));
const packages = metadata.packages.filter((pkg) => pkg.source).map((pkg) => ({
  ecosystem: 'cargo', name: pkg.name, version: pkg.version, license: pkg.license,
  repository: pkg.repository, directory: dirname(pkg.manifest_path), licenseFile: pkg.license_file,
}));
const visited = new Set();
function npmDependencies(manifest) {
  manifest = realpathSync(manifest);
  const json = JSON.parse(readFileSync(manifest, 'utf8'));
  const require = createRequire(manifest);
  for (const name of Object.keys(json.dependencies || {})) {
    const file = require.resolve.paths(name).map((base) => join(base, name, 'package.json')).find(existsSync);
    if (!file) throw new Error(`Missing installed dependency: ${name}`);
    const dependency = JSON.parse(readFileSync(file, 'utf8'));
    const id = `${dependency.name}@${dependency.version}`;
    if (visited.has(id)) continue;
    visited.add(id);
    packages.push({ ecosystem: 'npm', name: dependency.name, version: dependency.version,
      license: dependency.license, repository: dependency.repository?.url || dependency.repository,
      directory: dirname(file) });
    npmDependencies(file);
  }
}
npmDependencies(join(root, 'package.json'));
function licenseFiles(directory, depth = 0) {
  const files = [];
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    if (entry.isFile() && /^(licen[cs]e|copying|notice|copyright|authors)([._-]|$)/i.test(entry.name)) files.push(path);
    if (entry.isDirectory() && depth < 2 && /^(licen[cs]es?|legal|notices?)$/i.test(entry.name)) {
      files.push(...licenseFiles(path, depth + 1));
      // License directories also contain texts named after SPDX identifiers.
      for (const file of readdirSync(path, { withFileTypes: true })) if (file.isFile()) files.push(join(path, file.name));
    }
  }
  return files;
}
rmSync(output, { recursive: true, force: true });
mkdirSync(output, { recursive: true });
const inventory = [];
for (const pkg of packages.sort((a, b) => `${a.ecosystem}/${a.name}`.localeCompare(`${b.ecosystem}/${b.name}`))) {
  const id = `${pkg.ecosystem}/${pkg.name.replaceAll('/', '__')}@${pkg.version}`;
  const files = new Set(licenseFiles(pkg.directory));
  if (pkg.licenseFile) files.add(resolve(pkg.directory, pkg.licenseFile));
  const copied = [];
  for (const file of files) {
    const path = relative(pkg.directory, file);
    if (path.startsWith('..')) throw new Error(`License outside package: ${pkg.name}`);
    const target = join(output, id, path);
    mkdirSync(dirname(target), { recursive: true });
    copyFileSync(file, target);
    copied.push(`${id}/${path}`);
  }
  const extra = overrides[id] || [];
  for (const item of extra) {
    const file = resolve(root, item.file);
    const content = readFileSync(file);
    if (createHash('sha256').update(content).digest('hex') !== item.sha256) throw new Error(`License checksum mismatch: ${item.file}`);
    const path = `${id}/${basename(file)}`;
    mkdirSync(dirname(join(output, path)), { recursive: true });
    copyFileSync(file, join(output, path));
    copied.push(path);
  }
  if (!copied.length) throw new Error(`Missing license files for ${id}; add an upstream notice to third-party/license-overrides.json`);
  let sourceArchive;
  if (pkg.ecosystem === 'cargo' && /MPL|GPL/.test(pkg.license || '')) {
    const archive = join(dirname(dirname(dirname(pkg.directory))), 'cache', basename(dirname(pkg.directory)), `${pkg.name}-${pkg.version}.crate`);
    sourceArchive = `sources/${pkg.name}-${pkg.version}.crate`;
    mkdirSync(join(output, 'sources'), { recursive: true });
    copyFileSync(archive, join(output, sourceArchive));
  }
  inventory.push({ package: id, license: pkg.license, repository: pkg.repository, files: copied,
    upstreamNotices: extra.length ? extra.map((item) => item.source) : undefined, sourceArchive });
}
writeFileSync(join(output, 'inventory.json'), JSON.stringify(inventory, null, 2) + '\n');
writeFileSync(join(output, 'README.txt'), 'Third-party dependency notices for Mukure.\n\nThe inventory lists package versions, declared licenses, upstream repositories and the license/notice files supplied by each package. Dependencies retain their original licenses. FFmpeg and x264 sources and licenses are distributed separately in third-party/media/.\n');
console.log(`Collected licensing metadata and supplied notices for ${inventory.length} dependencies.`);
