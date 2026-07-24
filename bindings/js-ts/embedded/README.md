# @wardrobe/embedded

MIT-licensed native JavaScript and TypeScript binding for local Wardrobe storage. Current version: `0.26.725`; Node.js 24 or newer is required.

```js
const { WardrobeClient, relationshipRequest } = require('@wardrobe/embedded');

const root = await WardrobeClient.open('./wardrobe');
await root.create({ Database: { database_name: 'publishing-house' } });
await root.create({
  Schema: { database_name: 'publishing-house', schema_name: 'public' }
});
await root.create({
  Drawer: {
    database_name: 'publishing-house',
    schema_name: 'public',
    drawer_name: 'book'
  }
});

const wardrobe = await WardrobeClient.open('./wardrobe/publishing-house/public');
await wardrobe.alter(relationshipRequest('character', 'item_map', 'item'));
await wardrobe.upsert({
  _id: 'hero',
  attributes: { strength: 18, proficiencies: ['athletics'] }
}, 'character');
await wardrobe.upsert({ _id: 'book-01', title: 'The Lantern Index' }, 'book');
const records = await wardrobe.read('book');
const databases = await root.status('Databases');
console.log({ databases, records });
```

Database, schema, and drawer status requests return arrays directly.

The embedded package loads the local Node-API library and does not connect to a local Wardrobe server.
