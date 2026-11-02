// Trusted version policy. Candidate files are data, never executable build scripts.
const VERSION = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;
const SHA = /^[a-f0-9]{40}$/;
const VERSION_FILES = ['package.json', 'package-lock.json', 'src-tauri/tauri.conf.json', 'src-tauri/Cargo.toml', 'Cargo.lock'];

function assertVersion(value) {
  if (typeof value !== 'string' || !VERSION.test(value)) throw new Error('package.json must use a stable numeric version');
  return value;
}

function compareVersions(left, right) {
  const a = assertVersion(left).split('.').map(BigInt);
  const b = assertVersion(right).split('.').map(BigInt);
  for (let i = 0; i < 3; i += 1) if (a[i] !== b[i]) return a[i] > b[i] ? 1 : -1;
  return 0;
}

function nextForkVersion(current, upstream) {
  const parts = assertVersion(current).split('.').map(BigInt);
  const source = assertVersion(upstream).split('.').map(BigInt);
  // Append a decimal 1 to the upstream patch. Numeric arithmetic keeps a zero
  // patch valid SemVer (1.4.0 -> 1.4.1, never 1.4.01).
  source[2] = source[2] * 10n + 1n;
  const mapped = source.join('.');
  if (compareVersions(mapped, current) > 0) return mapped;
  // Additional releases of that baseline must still update existing clients.
  parts[2] += 1n;
  return parts.join('.');
}

function forkUpstream(packageJson) {
  const value = packageJson.forkUpstream;
  if (value === undefined) return null;
  if (!value || typeof value !== 'object' || Array.isArray(value)
      || Object.keys(value).sort().join(',') !== 'commit,version'
      || typeof value.commit !== 'string' || !SHA.test(value.commit)) {
    throw new Error('forkUpstream must contain an exact upstream version and commit');
  }
  assertVersion(value.version);
  return value;
}

function cargoVersion(text, file, version) {
  const sections = file === 'Cargo.lock'
    ? /(^\[\[package\]\]\r?\n)([\s\S]*?)(?=^\[|$(?![\s\S]))/gm
    : /(^\[package\]\r?\n)([\s\S]*?)(?=^\[|$(?![\s\S]))/gm;
  let found = 0;
  let previous;
  const result = text.replace(sections, (whole, header, body) => {
    const names = [...body.matchAll(/^name\s*=\s*"([^"]+)"\s*(?:#.*)?$/gm)];
    if (names.length !== 1 || names[0][1] !== 'cockpit-tools') return whole;
    if (file === 'Cargo.lock' && /^source\s*=/m.test(body)) return whole;
    const versions = [...body.matchAll(/^version\s*=\s*"([^"]+)"\s*(?:#.*)?$/gm)];
    if (versions.length !== 1) throw new Error(`Expected one cockpit-tools version in ${file}`);
    found += 1;
    previous = assertVersion(versions[0][1]);
    return header + body.replace(/^(version\s*=\s*")[^"]+("[^\r\n]*)$/m,
      (_line, prefix, suffix) => `${prefix}${version}${suffix}`);
  });
  if (found !== 1) throw new Error(`Expected one local cockpit-tools package in ${file}`);
  return { text: result, versions: [previous] };
}

function versionFile(file, text, version, upstream) {
  assertVersion(version);
  if (file.endsWith('.toml') || file === 'Cargo.lock') return cargoVersion(text, file, version);
  const value = JSON.parse(text);
  const versions = [assertVersion(value.version)];
  value.version = version;
  if (file === 'package.json') {
    if (value.name !== 'cockpit-tools') throw new Error('Unexpected application package name');
    if (upstream) value.forkUpstream = forkUpstream({ forkUpstream: upstream });
    else delete value.forkUpstream;
  } else if (file === 'package-lock.json') {
    if (value.name !== 'cockpit-tools' || value.packages?.['']?.name !== 'cockpit-tools') {
      throw new Error('Expected the application root in package-lock.json');
    }
    versions.push(assertVersion(value.packages[''].version));
    value.packages[''].version = version;
  } else if (file !== 'src-tauri/tauri.conf.json') {
    throw new Error('Unexpected version file');
  }
  return { text: `${JSON.stringify(value, null, 2)}\n`, versions };
}

module.exports = { VERSION, VERSION_FILES, assertVersion, compareVersions, nextForkVersion, forkUpstream, versionFile };
