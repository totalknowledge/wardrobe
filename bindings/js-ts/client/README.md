# @wardrobe/client

MIT-licensed JavaScript and TypeScript client for Wardrobe servers over TCP or Unix sockets. Current version: `0.26.723`; Node.js 24 or newer is required.

```js
const { WardrobeClient } = require('@wardrobe/client');

const client = await WardrobeClient.open('wardrobe://127.0.0.1:24842');
const databases = await client.status('Databases');
const schemas = await client.status({ Schemas: { database_name: 'publishing-house' } });
const drawers = await client.status({
  Drawers: { database_name: 'publishing-house', schema_name: 'public' }
});
const records = await client.read(['book', { page_count: 420 }]);
console.log({ databases, schemas, drawers, records });
await client.close();
```

Database, schema, and drawer status requests return arrays directly.

This package contains no embedded storage engine. Use `@wardrobe/embedded` when the process should own local Wardrobe files directly.
