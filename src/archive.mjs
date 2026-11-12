import { createReadStream } from 'node:fs';
import * as fs from 'node:fs/promises';
import path from 'node:path';
import { crc32 } from 'node:zlib';
import { Problem, visibleName, walk } from './files.mjs';

const fileLimit = 0xffffffff;
const flags = 0x0808;

function dosTime(date) {
  const year = Math.min(2107, Math.max(1980, date.getFullYear()));
  return {
    date: ((year - 1980) << 9) | ((date.getMonth() + 1) << 5) | date.getDate(),
    time: (date.getHours() << 11) | (date.getMinutes() << 5) | Math.floor(date.getSeconds() / 2),
  };
}

// ZIP32 with streaming CRCs. ZIPs contain regular files, never directory links.
export async function zipFolder(root, output, { source = false, signal } = {}) {
  const { entries, truncated } = await walk(root, { include: source ? visibleName : undefined, maxEntries: 20000 });
  if (truncated) throw new Problem(413, 'The export contains too many files for a single ZIP.');
  const handle = await fs.open(output, 'wx', 0o600);
  let position = 0;
  const directory = [];
  async function write(buffer) {
    signal?.throwIfAborted();
    if (position + buffer.length >= fileLimit) throw new Problem(413, 'ZIP exports are limited to 4 GiB.');
    await handle.writeFile(buffer);
    position += buffer.length;
  }
  try {
    for (const entry of entries) {
      const name = Buffer.from(entry.path);
      if (name.length > 65535) throw new Problem(413, 'An export path is too long for ZIP.');
      const file = path.join(root, ...entry.path.split('/'));
      const stat = await fs.lstat(file);
      if (!stat.isFile()) continue;
      const { date, time } = dosTime(stat.mtime);
      const offset = position;
      const local = Buffer.alloc(30);
      local.writeUInt32LE(0x04034b50, 0);
      local.writeUInt16LE(20, 4);
      local.writeUInt16LE(flags, 6);
      local.writeUInt16LE(time, 10);
      local.writeUInt16LE(date, 12);
      local.writeUInt16LE(name.length, 26);
      await write(local);
      await write(name);
      let crc = 0;
      let size = 0;
      for await (const chunk of createReadStream(file, { signal })) {
        crc = crc32(chunk, crc);
        size += chunk.length;
        await write(chunk);
      }
      const descriptor = Buffer.alloc(16);
      descriptor.writeUInt32LE(0x08074b50, 0);
      descriptor.writeUInt32LE(crc, 4);
      descriptor.writeUInt32LE(size, 8);
      descriptor.writeUInt32LE(size, 12);
      await write(descriptor);
      directory.push({ name, crc, size, offset, date, time, mode: stat.mode });
    }
    const centralOffset = position;
    for (const item of directory) {
      const central = Buffer.alloc(46);
      central.writeUInt32LE(0x02014b50, 0);
      central.writeUInt16LE(0x0314, 4);
      central.writeUInt16LE(20, 6);
      central.writeUInt16LE(flags, 8);
      central.writeUInt16LE(item.time, 12);
      central.writeUInt16LE(item.date, 14);
      central.writeUInt32LE(item.crc, 16);
      central.writeUInt32LE(item.size, 20);
      central.writeUInt32LE(item.size, 24);
      central.writeUInt16LE(item.name.length, 28);
      central.writeUInt32LE((item.mode << 16) >>> 0, 38);
      central.writeUInt32LE(item.offset, 42);
      await write(central);
      await write(item.name);
    }
    const end = Buffer.alloc(22);
    end.writeUInt32LE(0x06054b50, 0);
    end.writeUInt16LE(directory.length, 8);
    end.writeUInt16LE(directory.length, 10);
    end.writeUInt32LE(position - centralOffset, 12);
    end.writeUInt32LE(centralOffset, 16);
    await write(end);
    await handle.close();
  } catch (error) {
    await handle.close();
    await fs.rm(output, { force: true });
    throw error;
  }
}
