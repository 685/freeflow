import { createReadStream } from 'node:fs';
import * as fs from 'node:fs/promises';
import http from 'node:http';
import path from 'node:path';
import { inside } from './files.mjs';

const types = {
  '.html': 'text/html; charset=utf-8', '.js': 'text/javascript; charset=utf-8',
  '.mjs': 'text/javascript; charset=utf-8', '.json': 'application/json', '.bin': 'application/octet-stream',
  '.wasm': 'application/wasm', '.css': 'text/css', '.png': 'image/png', '.ico': 'image/x-icon',
  '.jpg': 'image/jpeg', '.jpeg': 'image/jpeg', '.svg': 'image/svg+xml', '.webp': 'image/webp',
  '.woff2': 'font/woff2', '.ttf': 'font/ttf', '.otf': 'font/otf',
};

export async function servePreview(root) {
  let folder = await fs.realpath(root);
  let origin;
  const server = http.createServer(async (req, res) => {
    res.setHeader('Cache-Control', 'no-store');
    res.setHeader('X-Content-Type-Options', 'nosniff');
    try {
      if (req.headers.host !== new URL(origin).host || !['GET', 'HEAD'].includes(req.method)) {
        res.writeHead(403).end();
        return;
      }
      const url = new URL(req.url, origin);
      const relative = decodeURIComponent(url.pathname).replace(/^\/+/, '') || 'index.html';
      if (/[\\:\0]/.test(relative)) {
        res.writeHead(400).end();
        return;
      }
      const base = folder;
      const candidate = path.resolve(base, relative);
      if (!inside(base, candidate)) {
        res.writeHead(403).end();
        return;
      }
      const direct = await fs.realpath(candidate).catch(() => null);
      const fallback = !path.extname(relative) ? path.join(base, 'index.html') : null;
      const file = direct || fallback;
      if (!file || !inside(base, file)) {
        res.writeHead(404).end();
        return;
      }
      const stat = await fs.stat(file);
      if (!stat.isFile()) {
        res.writeHead(404).end();
        return;
      }
      res.writeHead(200, {
        'Content-Type': types[path.extname(file).toLowerCase()] || 'application/octet-stream',
        'Content-Length': stat.size,
      });
      if (req.method === 'HEAD') return res.end();
      const stream = createReadStream(file);
      stream.on('error', () => res.destroy());
      res.once('close', () => stream.destroy());
      stream.pipe(res);
    } catch {
      if (res.headersSent) res.destroy();
      else res.writeHead(404).end();
    }
  });
  await new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', () => {
      origin = 'http://127.0.0.1:' + server.address().port;
      resolve();
    });
  });
  return {
    url: origin + '/',
    async swap(next) { folder = await fs.realpath(next); },
    async close() {
      await new Promise(resolve => {
        server.close(resolve);
        server.closeAllConnections();
      });
    },
  };
}
