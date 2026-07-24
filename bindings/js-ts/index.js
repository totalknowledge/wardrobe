'use strict';

const { classifyConnectionTarget, DEFAULT_NETWORK_PORT, SUPPORTED_OPERATIONS } = require('./classify');

const PACKAGE_NAMES = Object.freeze(['@wardrobe/client', '@wardrobe/embedded']);
const PACKAGE_VERSION = '0.26.724';

module.exports = {
  DEFAULT_NETWORK_PORT,
  PACKAGE_NAMES,
  PACKAGE_VERSION,
  SUPPORTED_OPERATIONS,
  classifyConnectionTarget
};
