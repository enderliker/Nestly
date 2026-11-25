import { assertAssetName, SHA256_HEX } from './verify.mjs';

export class GitHubError extends Error {
  constructor(message) {
    super(message);
    this.name = 'GitHubError';
  }
}

/** Hosts a release download may resolve to, including GitHub's CDN. */
export const DEFAULT_ALLOWED_HOSTS = new Set([
  'github.com',
  'api.github.com',
  'objects.githubusercontent.com',
  'release-assets.githubusercontent.com',
]);

const RELEASE_TAG = /^v?\d+\.\d+\.\d+(?:-[0-9A-Za-z]+(?:[.-][0-9A-Za-z]+)*)?$/;
const BINARY_NAME = /^(?:nestly|rsbot)-(?:supervisor|edge|mod-[a-z0-9-]+)$/;
const REDIRECT_STATUS = new Set([301, 302, 303, 307, 308]);

function headersFor(token) {
  const headers = {
    accept: 'application/vnd.github+json',
    'user-agent': 'nestly-launcher/0.1',
  };
  if (token) {
    headers.authorization = `Bearer ${token}`;
  }
  return headers;
}

/**
 * Validates a URL before any request is made.
 *
 * @throws {GitHubError} for a malformed URL, plain HTTP in production, embedded
 * credentials, or a host outside the allowlist.
 */
export function assertAllowedUrl(raw, allowedHosts = DEFAULT_ALLOWED_HOSTS, options = {}) {
  const { requireHttps = true } = options;
  let url;
  try {
    url = new URL(raw);
  } catch {
    throw new GitHubError(`invalid URL: ${String(raw)}`);
  }
  if (requireHttps && url.protocol !== 'https:') {
    throw new GitHubError(`refusing a non-HTTPS URL: ${url.href}`);
  }
  if (url.username || url.password) {
    throw new GitHubError('refusing a URL with embedded credentials');
  }
  if (!allowedHosts.has(url.host)) {
    throw new GitHubError(`host ${url.host} is not in the download allowlist`);
  }
  return url;
}

async function readJson(response, label) {
  try {
    return await response.json();
  } catch (source) {
    throw new GitHubError(`${label} returned invalid JSON: ${source.message}`);
  }
}

/**
 * Resolves a release through the GitHub API.
 *
 * @returns {Promise<{tag: string, assets: Array<{name: string, url: string, size: number, digest: string|null}>}>}
 */
export async function resolveRelease(config, options = {}) {
  const {
    apiBase = 'https://api.github.com',
    allowedHosts = DEFAULT_ALLOWED_HOSTS,
    fetchImpl = fetch,
    requireHttps = true,
  } = options;

  const reference = config.pinVersion ?? config.release ?? 'latest';
  const suffix = reference === 'latest' ? 'latest' : `tags/${reference}`;
  const url = assertAllowedUrl(
    `${apiBase}/repos/${config.repo}/releases/${suffix}`,
    allowedHosts,
    { requireHttps },
  );

  let response;
  try {
    response = await fetchImpl(url, {
      redirect: 'manual',
      headers: headersFor(config.githubToken),
      signal: AbortSignal.timeout(config.httpTimeoutMs ?? 30_000),
    });
  } catch (source) {
    throw new GitHubError(`cannot reach the GitHub release API: ${source.message}`);
  }
  if (response.status >= 300 && response.status < 400) {
    throw new GitHubError('the release API must not redirect');
  }
  if (!response.ok) {
    throw new GitHubError(`the release API answered HTTP ${response.status}`);
  }

  const body = await readJson(response, 'the release API');
  const tag = body?.tag_name;
  if (typeof tag !== 'string' || !RELEASE_TAG.test(tag)) {
    throw new GitHubError(`invalid release tag: ${String(tag)}`);
  }
  if (!Array.isArray(body.assets)) {
    throw new GitHubError('the release has no asset list');
  }
  const assets = body.assets.map((asset) => {
    if (typeof asset?.name !== 'string' || typeof asset?.browser_download_url !== 'string') {
      throw new GitHubError('the release contains a malformed asset');
    }
    return {
      name: asset.name,
      url: asset.browser_download_url,
      size: Number(asset.size ?? 0),
      digest: asset.digest ?? null,
    };
  });
  return {
    tag,
    assets,
    manifest:
      assets.find((asset) => asset.name === 'nestly-manifest.json') ??
      assets.find((asset) => asset.name === 'rsbot-manifest.json') ??
      null,
    signature:
      assets.find((asset) => asset.name === 'nestly-manifest.json.sig') ??
      assets.find((asset) => asset.name === 'rsbot-manifest.json.sig') ??
      null,
  };
}

/**
 * Downloads one asset with redirect re-validation, a timeout, and a streaming
 * size cap.
 *
 * @returns {Promise<Buffer>}
 */
export async function downloadAsset(rawUrl, options = {}) {
  const {
    maxBytes,
    timeoutMs = 30_000,
    allowedHosts = DEFAULT_ALLOWED_HOSTS,
    fetchImpl = fetch,
    requireHttps = true,
    token = null,
  } = options;
  if (!Number.isInteger(maxBytes) || maxBytes < 1) {
    throw new GitHubError('a positive maxBytes limit is required');
  }

  let url = assertAllowedUrl(rawUrl, allowedHosts, { requireHttps });
  for (let hop = 0; hop <= 5; hop += 1) {
    let response;
    try {
      // Only GitHub itself receives the credential; CDN redirect targets use
      // their signed URL and reject an Authorization header.
      const credentialed = url.host === 'github.com' || url.host === 'api.github.com';
      response = await fetchImpl(url, {
        redirect: 'manual',
        headers: headersFor(credentialed ? token : null),
        signal: AbortSignal.timeout(timeoutMs),
      });
    } catch (source) {
      throw new GitHubError(`download failed: ${source.message}`);
    }

    if (REDIRECT_STATUS.has(response.status)) {
      const location = response.headers.get('location');
      if (!location) {
        throw new GitHubError('a redirect did not provide a location');
      }
      url = assertAllowedUrl(new URL(location, url).href, allowedHosts, { requireHttps });
      continue;
    }
    if (!response.ok) {
      throw new GitHubError(`download failed with HTTP ${response.status}`);
    }

    const declared = Number(response.headers.get('content-length'));
    if (Number.isFinite(declared) && declared > maxBytes) {
      await response.body?.cancel?.().catch(() => {});
      throw new GitHubError(`download exceeds the ${maxBytes} byte size cap`);
    }
    if (!response.body) {
      throw new GitHubError('the download response has no body');
    }

    const reader = response.body.getReader();
    const chunks = [];
    let total = 0;
    while (true) {
      let step;
      try {
        step = await reader.read();
      } catch (source) {
        throw new GitHubError(`download interrupted: ${source.message}`);
      }
      if (step.done) {
        break;
      }
      total += step.value.byteLength;
      if (total > maxBytes) {
        await reader.cancel().catch(() => {});
        throw new GitHubError(`download exceeds the ${maxBytes} byte size cap`);
      }
      chunks.push(Buffer.from(step.value));
    }
    return Buffer.concat(chunks);
  }
  throw new GitHubError('too many redirects');
}

/**
 * Validates the signed release manifest and returns the assets for one target.
 *
 * @throws {GitHubError} or {VerificationError} when the manifest is unsafe.
 */
export function validateManifest(manifest, { target, maxBytes }) {
  if (!manifest || typeof manifest !== 'object') {
    throw new GitHubError('the release manifest is not an object');
  }
  if (manifest.schema !== 1) {
    throw new GitHubError(`unsupported manifest schema: ${String(manifest.schema)}`);
  }
  if (typeof manifest.version !== 'string' || !RELEASE_TAG.test(manifest.version)) {
    throw new GitHubError(`invalid manifest version: ${String(manifest.version)}`);
  }
  if (
    !Number.isInteger(manifest.protocol) ||
    manifest.protocol < 1 ||
    manifest.protocol > 65_535
  ) {
    throw new GitHubError(`invalid manifest protocol: ${String(manifest.protocol)}`);
  }
  const selected = manifest.architectures?.[target];
  if (!selected || !Array.isArray(selected.assets) || selected.assets.length === 0) {
    throw new GitHubError(`the manifest has no assets for architecture ${target}`);
  }

  const assets = [];
  const files = new Set();
  for (const asset of selected.assets) {
    if (typeof asset?.name !== 'string' || !BINARY_NAME.test(asset.name)) {
      throw new GitHubError(`invalid asset name: ${String(asset?.name)}`);
    }
    assertAssetName(asset.file);
    if (typeof asset.sha256 !== 'string' || !SHA256_HEX.test(asset.sha256)) {
      throw new GitHubError(`invalid sha256 for ${asset.file}`);
    }
    if (
      !Number.isInteger(asset.size) ||
      asset.size < 0 ||
      asset.size > maxBytes
    ) {
      throw new GitHubError(`invalid size for ${asset.file}`);
    }
    if (files.has(asset.file)) {
      throw new GitHubError(`duplicate asset file: ${asset.file}`);
    }
    files.add(asset.file);
    assets.push({
      name: asset.name,
      file: asset.file,
      sha256: asset.sha256.toLowerCase(),
      size: asset.size,
    });
  }

  const names = new Set(assets.map((asset) => asset.name));
  for (const required of ['supervisor', 'edge']) {
    if (!names.has(`nestly-${required}`) && !names.has(`rsbot-${required}`)) {
      throw new GitHubError(`the manifest is missing the nestly-${required} asset`);
    }
  }

  return { version: manifest.version, protocol: manifest.protocol, assets };
}