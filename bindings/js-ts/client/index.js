const net = require('net');

const PACKAGE_NAME = '@wardrobe/client';
const PACKAGE_VERSION = '0.26.723';

function relationshipRequest(drawerName, fieldName, targetDrawer) {
  return {
    SchemaRule: {
      drawer_name: drawerName,
      action: 'add',
      kind: 'relationship',
      field_name: fieldName,
      payload: {
        type: 'M:1',
        target_drawer: targetDrawer
      }
    }
  };
}

function writeFrame(socket, opcode, payloadStr) {
  const payload = Buffer.from(payloadStr, 'utf8');
  const header = Buffer.alloc(7);
  header[0] = 0x57; // 'W'
  header[1] = 0x44; // 'D'
  header[2] = opcode;
  header.writeUInt32BE(payload.length, 3);
  socket.write(header);
  socket.write(payload);
}

function sendCommand(socket, command) {
  const payloadStr = JSON.stringify(command);
  writeFrame(socket, 0x01, payloadStr);

  return new Promise((resolve, reject) => {
    let buffer = Buffer.alloc(0);

    const onData = (chunk) => {
      buffer = Buffer.concat([buffer, chunk]);
      
      if (buffer.length >= 7) {
        const magic1 = buffer[0];
        const magic2 = buffer[1];
        if (magic1 !== 0x57 || magic2 !== 0x44) {
          cleanup();
          reject(new Error('Invalid Wardrobe protocol magic bytes'));
          return;
        }

        const opcode = buffer[2];
        const payloadLen = buffer.readUInt32BE(3);

        if (buffer.length >= 7 + payloadLen) {
          const payload = buffer.slice(7, 7 + payloadLen);
          const responseStr = payload.toString('utf8');
          cleanup();

          if (opcode === 0x02) { // Result
            try {
              const res = JSON.parse(responseStr);
              resolve(res);
            } catch (err) {
              reject(new Error(`Failed to deserialize Wardrobe command result: ${err.message}`));
            }
          } else if (opcode === 0x03) { // Error
            reject(new Error(responseStr));
          } else {
            reject(new Error(`Wardrobe server returned unexpected opcode: ${opcode}`));
          }
        }
      }
    };

    const onError = (err) => {
      cleanup();
      reject(err);
    };

    const onClose = () => {
      cleanup();
      reject(new Error('Connection closed'));
    };

    const cleanup = () => {
      socket.off('data', onData);
      socket.off('error', onError);
      socket.off('close', onClose);
    };

    socket.on('data', onData);
    socket.on('error', onError);
    socket.on('close', onClose);
  });
}

class WardrobeClient {
  constructor(socket) {
    this.socket = socket;
  }

  static async open(connectionString) {
    return new Promise((resolve, reject) => {
      let socket;
      if (connectionString.startsWith('wardrobe+unix://')) {
        const socketPath = connectionString.slice('wardrobe+unix://'.length);
        socket = net.createConnection({ path: socketPath });
      } else if (connectionString.startsWith('wardrobe://unix/')) {
        const socketPath = connectionString.slice('wardrobe://unix/'.length);
        socket = net.createConnection({ path: socketPath });
      } else if (connectionString.startsWith('wardrobe://')) {
        let authority = connectionString.slice('wardrobe://'.length).replace(/\/+$/, '');
        let host = authority;
        let port = 24842;
        if (authority.includes(':')) {
          const parts = authority.split(':');
          host = parts[0];
          port = parseInt(parts[1], 10);
        }
        socket = net.createConnection({ host, port });
      } else {
        reject(new Error(`Unsupported network connection string: ${connectionString}`));
        return;
      }

      socket.on('connect', () => {
        resolve(new WardrobeClient(socket));
      });

      socket.on('error', (err) => {
        reject(err);
      });
    });
  }

  async close() {
    if (this.socket) {
      this.socket.end();
    }
  }

  async execute(command) {
    return sendCommand(this.socket, command);
  }

  async upsert(payload, filter, options) {
    const res = await this.execute({
      upsert: {
        payload,
        filter: normalizeFilter(filter),
        options: normalizeOptions(options)
      }
    });
    return res.upsert;
  }

  async read(filter, options) {
    const res = await this.execute({
      read: {
        filter: normalizeFilter(filter),
        options: normalizeOptions(options)
      }
    });
    return unwrapReadResult(res.read);
  }

  async delete(filter, options) {
    const res = await this.execute({
      delete: {
        filter: normalizeFilter(filter),
        options: normalizeOptions(options)
      }
    });
    return res.delete;
  }

  async inspect(filter, options) {
    const res = await this.execute({
      inspect: {
        filter: normalizeFilter(filter),
        options: normalizeOptions(options)
      }
    });
    return res.inspect;
  }

  async count(filter, options) {
    const res = await this.execute({
      count: {
        filter: normalizeFilter(filter),
        options: normalizeOptions(options)
      }
    });
    return res.count;
  }

  async clean(request) {
    const res = await this.execute({
      compact: request || null
    });
    return res.compact;
  }

  async create(request) {
    const res = await this.execute({
      create: request
    });
    return res.create;
  }

  async alter(request) {
    const res = await this.execute({
      alter: request
    });
    return res.alter;
  }

  async drop(request) {
    const res = await this.execute({
      drop: request
    });
    return res.drop;
  }

  async backup(sourcePath) {
    const res = await this.execute({
      backup: { source_path: sourcePath }
    });
    return res.backup;
  }

  async restore(destinationPath, archive) {
    const res = await this.execute({
      restore: { destination_path: destinationPath, archive }
    });
    return res.restore;
  }

  async grant(request) {
    const res = await this.execute({
      grant: request
    });
    return res.grant;
  }

  async revoke(request) {
    const res = await this.execute({
      revoke: request
    });
    return res.revoke;
  }

  async status(request) {
    const res = await this.execute({
      status: request || "Storage"
    });
    return res.status;
  }
}

function unwrapReadResult(readResult) {
  if (!readResult || typeof readResult !== 'object' || Array.isArray(readResult)) {
    return readResult;
  }
  if (Object.prototype.hasOwnProperty.call(readResult, 'Records')) {
    return readResult.Records;
  }
  if (Object.prototype.hasOwnProperty.call(readResult, 'Page')) {
    return readResult.Page;
  }
  if (Object.prototype.hasOwnProperty.call(readResult, 'records')) {
    return readResult.records;
  }
  if (Object.prototype.hasOwnProperty.call(readResult, 'page')) {
    return readResult.page;
  }
  if (Object.prototype.hasOwnProperty.call(readResult, 'Record')) {
    return readResult.Record;
  }
  if (Object.prototype.hasOwnProperty.call(readResult, 'record')) {
    return readResult.record;
  }
  if (Object.prototype.hasOwnProperty.call(readResult, 'Pointers')) {
    return readResult.Pointers;
  }
  if (Object.prototype.hasOwnProperty.call(readResult, 'pointers')) {
    return readResult.pointers;
  }
  if (Object.prototype.hasOwnProperty.call(readResult, 'Exists')) {
    return readResult.Exists;
  }
  if (Object.prototype.hasOwnProperty.call(readResult, 'exists')) {
    return readResult.exists;
  }
  return readResult;
}

function normalizeFilter(filter) {
  if (!filter) return "None";
  if (Array.isArray(filter)) {
    return { Many: filter.map(normalizeFilter) };
  }
  if (typeof filter === 'string') {
    if (filter.startsWith('@')) {
      if (filter.includes(':')) {
        return { Pointer: filter };
      }
      return { Drawer: filter.slice(1) };
    }
    return { Drawer: filter };
  }
  if (filter.drawer) {
    return { Drawer: filter.drawer };
  }
  if (filter.pointer) {
    return { Pointer: filter.pointer };
  }
  if (filter.query) {
    return { Query: filter.query };
  }
  return { Query: filter };
}

function normalizeOptions(options) {
  if (!options) return {};
  return {
    multi: options.multi !== undefined ? options.multi : null,
    atomic: options.atomic !== undefined ? options.atomic : null,
    create_if_missing: options.createIfMissing !== undefined ? options.createIfMissing : null,
    return_shape: options.returnShape !== undefined ? normalizeReturnShape(options.returnShape) : null,
    hydrate: options.hydrate !== undefined ? options.hydrate : null,
    limit: options.limit !== undefined ? options.limit : null,
    offset: options.offset !== undefined ? options.offset : null,
    order_by: options.orderBy !== undefined ? options.orderBy : null,
    order_direction: options.orderDirection !== undefined ? normalizeOrderDirection(options.orderDirection) : null,
    cursor: options.cursor !== undefined ? options.cursor : null,
    page: options.page !== undefined ? options.page : null,
    page_size: options.pageSize !== undefined ? options.pageSize : null,
    include_diagnostics: options.includeDiagnostics !== undefined ? options.includeDiagnostics : null
  };
}

function normalizeReturnShape(returnShape) {
  const shapes = {
    default: 'Default',
    records: 'Records',
    record: 'Record',
    pointers: 'Pointers',
    exists: 'Exists',
    diagnostics: 'Diagnostics'
  };
  return shapes[returnShape.toLowerCase()] || returnShape;
}

function normalizeOrderDirection(orderDirection) {
  const directions = {
    asc: 'Ascending',
    ascending: 'Ascending',
    desc: 'Descending',
    descending: 'Descending'
  };
  return directions[orderDirection.toLowerCase()] || orderDirection;
}

module.exports = { PACKAGE_NAME, PACKAGE_VERSION, WardrobeClient, relationshipRequest };
