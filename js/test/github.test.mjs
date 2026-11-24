import assert from 'node:assert/strict';
import http from 'node:http';
import test from 'node:test';

import {
  DEFAULT_ALLOWED_HOSTS,
  GitHubError,
  assertAllowedUrl,
  downloadAsset,
  resolveRelease,
  validateManifest,
} from '../lib/github.mjs';

async function startServer(handler) {
  const server = http.createServer(handler);
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  const { port } = server.address();
  return {
    url: `http://127.0.0.1:${port}`,
    host: `127.0.0.1:${port}`,
    close: () => {
      server.closeAllConnections?.();
      return new Promise((resolve) => server.close(resolve));
    },
  };
}

function releasePayload(base) {
  return {
    tag_name: 'v0.1.0',
    assets: [
      {
        name: 'nestly-manifest.json',
        size: 10,
        browser_download_url: `${base}/assets/manifest`,
        digest: `sha256:${'a'.repeat(64)}`,
      },
      {
        name: 'nestly-manifest.json.sig',
        size: 64,
        browser_download_url: `${base}/assets/signature`,
      },
    ],
  };
}

const MANIFEST = {
  schema: 1,
  version: '0.1.0',
  protocol: 1,
  architectures: {
    'x86_64-unknown-linux-musl': {
      assets: [
        {
          name: 'nestly-supervisor',
          file: 'nestly-supervisor-x86_64-unknown-linux-musl',
          sha256: 'b'.repeat(64),
          size: 1024,
        },
        {
          name: 'nestly-edge',
          file: 'nestly-edge-x86_64-unknown-linux-musl',
          sha256: 'c'.repeat(64),
          size: 2048,
        },
      ],
    },
  },
};

test('resolves a release and links its manifest assets', async () => {
  const server = await startServer((request, response) => {
    assert.equal(request.url, '/repos/enderliker/nestly/releases/latest');
    response.writeHead(200, { 'content-type': 'application/json' });
    response.end(JSON.stringify(releasePayload(server.url)));
  });
  try {
    const release = await resolveRelease(
      { repo: 'enderliker/nestly', release: 'latest', githubToken: null, httpTimeoutMs: 2_000 },
      {
        apiBase: server.url,
        allowedHosts: new Set([server.host]),
        requireHttps: false,
      },
    );
    assert.equal(release.tag, 'v0.1.0');
    assert.equal(release.assets.length, 2);
    assert.equal(release.manifest.url, `${server.url}/assets/manifest`);
  } finally {
    await server.close();
  }
});

test('rejects redirects to hosts outside the allowlist', async () => {
  const target = await startServer((request, response) => {
    response.writeHead(200);
    response.end('redirected');
  });
  const origin = await startServer((request, response) => {
    response.writeHead(302, { location: `${target.url}/evil` });
    response.end();
  });
  try {
    await assert.rejects(
      () =>
        downloadAsset(`${origin.url}/asset`, {
          maxBytes: 1024,
          timeoutMs: 2_000,
          allowedHosts: new Set([origin.host]),
          requireHttps: false,
        }),
      /allowlist/,
    );
  } finally {
    await origin.close();
    await target.close();
  }
});

test('enforces the download size cap while streaming', async () => {
  const server = await startServer((request, response) => {
    response.writeHead(200, { 'content-length': '4096' });
    response.end(Buffer.alloc(4096, 1));
  });
  try {
    await assert.rejects(
      () =>
        downloadAsset(`${server.url}/asset`, {
          maxBytes: 100,
          timeoutMs: 2_000,
          allowedHosts: new Set([server.host]),
          requireHttps: false,
        }),
      /size/,
    );
  } finally {
    await server.close();
  }
});

test('aborts a stalled download', async () => {
  const server = await startServer(() => {});
  try {
    await assert.rejects(
      () =>
        downloadAsset(`${server.url}/asset`, {
          maxBytes: 1024,
          timeoutMs: 100,
          allowedHosts: new Set([server.host]),
          requireHttps: false,
        }),
      /timed out|aborted/i,
    );
  } finally {
    await server.close();
  }
});

test('rejects plain HTTP and non-allowlisted hosts in production', () => {
  assert.throws(() => assertAllowedUrl('http://github.com/x', DEFAULT_ALLOWED_HOSTS), GitHubError);
  assert.throws(() => assertAllowedUrl('https://evil.example/x', DEFAULT_ALLOWED_HOSTS), GitHubError);
  assert.throws(() => assertAllowedUrl('file:///etc/passwd', DEFAULT_ALLOWED_HOSTS), GitHubError);
  assert.throws(() => assertAllowedUrl('https://user:pass@github.com/x', DEFAULT_ALLOWED_HOSTS), GitHubError);
  assert.equal(
    assertAllowedUrl('https://github.com/enderliker/nestly/releases', DEFAULT_ALLOWED_HOSTS).host,
    'github.com',
  );
});

test('validates the manifest schema strictly', () => {
  const validated = validateManifest(MANIFEST, {
    target: 'x86_64-unknown-linux-musl',
    maxBytes: 64 * 1024 * 1024,
  });
  assert.equal(validated.version, '0.1.0');
  assert.equal(validated.assets.length, 2);

  const cases = [
    [{ ...MANIFEST, schema: 2 }, /schema/],
    [{ ...MANIFEST, version: '../x' }, /version/],
    [{ ...MANIFEST, architectures: {} }, /architecture/],
    [
      {
        ...MANIFEST,
        architectures: {
          'x86_64-unknown-linux-musl': {
            assets: [MANIFEST.architectures['x86_64-unknown-linux-musl'].assets[0]],
          },
        },
      },
      /nestly-edge/,
    ],
    [
      {
        ...MANIFEST,
        architectures: {
          'x86_64-unknown-linux-musl': {
            assets: [
              { ...MANIFEST.architectures['x86_64-unknown-linux-musl'].assets[0], sha256: 'x' },
              MANIFEST.architectures['x86_64-unknown-linux-musl'].assets[1],
            ],
          },
        },
      },
      /sha256/,
    ],
    [
      {
        ...MANIFEST,
        architectures: {
          'x86_64-unknown-linux-musl': {
            assets: [
              {
                ...MANIFEST.architectures['x86_64-unknown-linux-musl'].assets[0],
                file: '../escape',
              },
              MANIFEST.architectures['x86_64-unknown-linux-musl'].assets[1],
            ],
          },
        },
      },
      /invalid/,
    ],
    [
      {
        ...MANIFEST,
        architectures: {
          'x86_64-unknown-linux-musl': {
            assets: [
              { ...MANIFEST.architectures['x86_64-unknown-linux-musl'].assets[0], size: 10 ** 12 },
              MANIFEST.architectures['x86_64-unknown-linux-musl'].assets[1],
            ],
          },
        },
      },
      /size/,
    ],
  ];
  for (const [manifest, pattern] of cases) {
    assert.throws(
      () => validateManifest(manifest, { target: 'x86_64-unknown-linux-musl', maxBytes: 64 * 1024 * 1024 }),
      pattern,
    );
  }
});