'use strict';

const DEFAULT_NETWORK_PORT = 24842;
const SUPPORTED_OPERATIONS = Object.freeze([
  'read',
  'upsert',
  'delete',
  'inspect',
  'count',
  'clean',
  'create',
  'alter',
  'drop',
  'backup',
  'restore',
  'grant',
  'revoke',
  'status'
]);

function classifyConnectionTarget(connectionString) {
  if (typeof connectionString !== 'string') {
    throw new TypeError('Wardrobe connection string must be a string');
  }

  const target = connectionString.trim();
  if (target.length === 0) {
    throw new TypeError('Wardrobe connection string cannot be empty');
  }

  if (target.startsWith('wardrobe://local/')) {
    return embeddedTarget(target.slice('wardrobe://local/'.length));
  }

  if (target.startsWith('wardrobe+file://')) {
    return embeddedTarget(target.slice('wardrobe+file://'.length));
  }

  if (target.startsWith('file://')) {
    return embeddedTarget(target.slice('file://'.length));
  }

  if (target.startsWith('wardrobe+unix://')) {
    return unixSocketTarget(target.slice('wardrobe+unix://'.length));
  }

  if (target.startsWith('wardrobe://unix/')) {
    return unixSocketTarget(target.slice('wardrobe://unix/'.length));
  }

  if (target.startsWith('wardrobe://')) {
    return networkTarget(target.slice('wardrobe://'.length));
  }

  if (target.includes('://')) {
    throw new TypeError(`Unsupported Wardrobe connection scheme: ${target}`);
  }

  return embeddedTarget(target);
}

function embeddedTarget(path) {
  const normalizedPath = normalizeUriPath(path);
  if (normalizedPath.length === 0) {
    throw new TypeError('Embedded Wardrobe connection URI requires a file-system path');
  }

  return Object.freeze({
    kind: 'embedded',
    path: normalizedPath,
    requiresEmbeddedEngine: true,
    usesSocketTransport: false
  });
}

function unixSocketTarget(path) {
  const normalizedPath = normalizeUriPath(path);
  if (normalizedPath.length === 0) {
    throw new TypeError('Unix socket Wardrobe connection URI requires a socket path');
  }

  return Object.freeze({
    kind: 'unix-socket',
    path: normalizedPath,
    requiresEmbeddedEngine: false,
    usesSocketTransport: true
  });
}

function networkTarget(authority) {
  const trimmedAuthority = authority.replace(/^\/+|\/+$/g, '');
  if (trimmedAuthority.length === 0) {
    throw new TypeError('Network Wardrobe connection URI requires a host');
  }

  if (trimmedAuthority.includes('/')) {
    throw new TypeError('Network Wardrobe connection URI should not contain a path');
  }

  const separatorIndex = trimmedAuthority.lastIndexOf(':');
  if (separatorIndex === -1) {
    return networkResult(trimmedAuthority, DEFAULT_NETWORK_PORT);
  }

  const host = trimmedAuthority.slice(0, separatorIndex);
  const portText = trimmedAuthority.slice(separatorIndex + 1);
  if (host.length === 0) {
    throw new TypeError('Network Wardrobe connection URI requires a host before the port');
  }

  const port = Number(portText);
  if (!Number.isInteger(port) || port < 0 || port > 65535) {
    throw new TypeError(`Invalid Wardrobe network port '${portText}'`);
  }

  return networkResult(host, port);
}

function networkResult(host, port) {
  return Object.freeze({
    kind: 'network',
    host,
    port,
    requiresEmbeddedEngine: false,
    usesSocketTransport: true
  });
}

function normalizeUriPath(path) {
  const trimmedPath = path.replace(/^\/+/, '');
  if (/^[A-Za-z]:/.test(trimmedPath)) {
    return trimmedPath;
  }

  return path.startsWith('/') ? `/${trimmedPath}` : trimmedPath;
}

module.exports = {
  DEFAULT_NETWORK_PORT,
  SUPPORTED_OPERATIONS,
  classifyConnectionTarget
};
