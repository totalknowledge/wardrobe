'use strict';

const { classifyConnectionTarget, DEFAULT_NETWORK_PORT, SUPPORTED_OPERATIONS } = require('./classify');

const PACKAGE_NAME = '@wardrobe/database';
const PACKAGE_VERSION = '0.1.0';

class WardrobeClient {
  static async open(connectionString) {
    const target = classifyConnectionTarget(connectionString);
    if (target.requiresEmbeddedEngine) {
      const { WardrobeClient: EmbeddedClient } = require('./embedded');
      return EmbeddedClient.open(connectionString);
    } else {
      const { WardrobeClient: NetworkClient } = require('./client');
      return NetworkClient.open(connectionString);
    }
  }
}

module.exports = {
  WardrobeClient,
  DEFAULT_NETWORK_PORT,
  PACKAGE_NAME,
  PACKAGE_VERSION,
  SUPPORTED_OPERATIONS,
  classifyConnectionTarget
};
