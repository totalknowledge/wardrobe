import { TestBed } from '@angular/core/testing';

import { WardrobeService } from './wardrobe-service';

describe('WardrobeService', () => {
  let service: WardrobeService;
  let invokeMock: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    localStorage.clear();
    document.documentElement.classList.remove('app-busy');
    document.body.classList.remove('app-busy');
    invokeMock = vi.fn().mockResolvedValue([]);
    (globalThis as any).__TAURI__ = {
      core: { invoke: invokeMock },
    };
    TestBed.configureTestingModule({});
    service = TestBed.inject(WardrobeService);
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it('should be created', () => {
    expect(service).toBeTruthy();
    expect(service.databaseStatus()).toBe('disconnected');
    expect(service.activeConnectionPath()).toBeNull();
  });

  it('forwards source creation and connection tests to Tauri', async () => {
    invokeMock.mockResolvedValue(undefined);

    await service.createSourceLocation('/data/wardrobe');
    await service.testConnection('wardrobe://server:24842');

    expect(invokeMock).toHaveBeenNthCalledWith(1, 'wardrobe_create_source_location', {
      databaseDirectory: '/data/wardrobe',
    });
    expect(invokeMock).toHaveBeenNthCalledWith(2, 'wardrobe_test_database_access', {
      databaseDirectory: 'wardrobe://server:24842',
    });
  });

  it('connects, loads wardrobes, and clears the busy state', async () => {
    const databases = [{ name: 'closet' }];
    invokeMock.mockImplementation((command: string) => Promise.resolve(
      command === 'wardrobe_show_wardrobes' ? databases : undefined,
    ));

    const connection = service.connectSourceLocation('/data/wardrobe', 'Local');

    expect(service.databaseStatus()).toBe('connecting');
    expect(document.body.classList.contains('app-busy')).toBe(true);

    await expect(connection).resolves.toEqual(databases);
    expect(service.databaseStatus()).toBe('connected');
    expect(service.activeConnectionPath()).toBe('/data/wardrobe');
    expect(service.databases()).toEqual(databases);
    expect(document.documentElement.classList.contains('app-busy')).toBe(false);
    expect(invokeMock).toHaveBeenCalledWith('wardrobe_connect_source_location', {
      databaseDirectory: '/data/wardrobe',
      name: 'Local',
    });
  });

  it('records connection failures and clears the busy state', async () => {
    const error = new Error('offline');
    const consoleError = vi.spyOn(console, 'error').mockImplementation(() => undefined);
    invokeMock.mockRejectedValue(error);

    await expect(service.connectSourceLocation('wardrobe://offline')).rejects.toBe(error);

    expect(service.databaseStatus()).toBe('error');
    expect(service.testDatabaseAccessError()).toBe('offline');
    expect(document.body.classList.contains('app-busy')).toBe(false);
    expect(consoleError).toHaveBeenCalled();
  });

  it('disconnects and clears loaded and persisted selection state', () => {
    service.databaseStatus.set('connected');
    service.activeConnectionPath.set('/data/wardrobe');
    service.databases.set([{ name: 'closet' }]);
    service.bays.set(['shelf']);
    service.drawers.set([{ name: 'shirts' }]);
    service.setSelectedContext('closet', 'shelf');

    service.disconnect();

    expect(service.databaseStatus()).toBe('disconnected');
    expect(service.activeConnectionPath()).toBeNull();
    expect(service.databases()).toEqual([]);
    expect(service.bays()).toEqual([]);
    expect(service.drawers()).toEqual([]);
    expect(service.selectedDatabaseName()).toBeNull();
    expect(service.selectedSchemaName()).toBeNull();
    expect(localStorage.getItem('selected_database')).toBeNull();
    expect(localStorage.getItem('selected_schema')).toBeNull();
  });

  it('loads wardrobes and recovers from loading failures', async () => {
    invokeMock.mockResolvedValueOnce([{ name: 'closet' }]);
    await expect(service.showWardrobes()).resolves.toEqual([{ name: 'closet' }]);
    expect(service.databases()).toEqual([{ name: 'closet' }]);
    expect(service.isLoadingDatabases()).toBe(false);

    vi.spyOn(console, 'error').mockImplementation(() => undefined);
    invokeMock.mockRejectedValueOnce(new Error('failed'));
    await expect(service.showWardrobes()).resolves.toEqual([]);
    expect(service.databases()).toEqual([]);
    expect(service.isLoadingDatabases()).toBe(false);
  });

  it('loads bays and clears them when loading fails', async () => {
    invokeMock.mockResolvedValueOnce(['shelf']);
    service.showBays('closet');
    await vi.waitFor(() => expect(service.bays()).toEqual(['shelf']));
    expect(service.isLoadingBays()).toBe(false);

    vi.spyOn(console, 'error').mockImplementation(() => undefined);
    invokeMock.mockRejectedValueOnce(new Error('failed'));
    service.showBays('closet');
    await vi.waitFor(() => expect(service.isLoadingBays()).toBe(false));
    expect(service.bays()).toEqual([]);
  });

  it('falls back from the default bay to public when loading drawers', async () => {
    invokeMock
      .mockResolvedValueOnce([])
      .mockResolvedValueOnce([{ name: 'shirts' }]);

    service.showDrawers('closet', 'default');

    await vi.waitFor(() => expect(service.drawers()).toEqual([{ name: 'shirts' }]));
    expect(invokeMock).toHaveBeenNthCalledWith(2, 'wardrobe_show_drawers', {
      databaseName: 'closet',
      schemaName: 'public',
    });
    expect(service.isLoadingDrawers()).toBe(false);
  });

  it('loads direct drawer results and clears failures', async () => {
    invokeMock.mockResolvedValueOnce([{ name: 'shirts' }]);
    service.showDrawers('closet', 'shelf');
    await vi.waitFor(() => expect(service.drawers()).toEqual([{ name: 'shirts' }]));

    vi.spyOn(console, 'error').mockImplementation(() => undefined);
    invokeMock.mockRejectedValueOnce(new Error('failed'));
    service.showDrawers('closet', 'shelf');
    await vi.waitFor(() => expect(service.isLoadingDrawers()).toBe(false));
    expect(service.drawers()).toEqual([]);
  });

  it('creates bays and drawers and refreshes their lists', async () => {
    invokeMock
      .mockResolvedValueOnce(undefined)
      .mockResolvedValueOnce(['shelf'])
      .mockResolvedValueOnce(undefined)
      .mockResolvedValueOnce([{ name: 'shirts' }]);

    service.createNewBay('closet', 'shelf');
    await vi.waitFor(() => expect(service.bays()).toEqual(['shelf']));
    service.createNewDrawer('closet', 'shelf', 'shirts');
    await vi.waitFor(() => expect(service.drawers()).toEqual([{ name: 'shirts' }]));

    expect(invokeMock).toHaveBeenCalledWith('wardrobe_create_new_bay', {
      databaseName: 'closet',
      schemaName: 'shelf',
    });
    expect(invokeMock).toHaveBeenCalledWith('wardrobe_create_new_drawer', {
      databaseName: 'closet',
      schemaName: 'shelf',
      drawerName: 'shirts',
    });
  });

  it('reports failed bay and drawer creation without leaving rejected promises', async () => {
    const consoleError = vi.spyOn(console, 'error').mockImplementation(() => undefined);
    invokeMock.mockRejectedValue(new Error('failed'));

    service.createNewBay('closet', 'shelf');
    service.createNewDrawer('closet', 'shelf', 'shirts');

    await vi.waitFor(() => expect(consoleError).toHaveBeenCalledTimes(2));
  });

  it('persists and clears selected database context', () => {
    service.setSelectedContext('closet', 'shelf');

    expect(service.selectedDatabaseName()).toBe('closet');
    expect(service.selectedSchemaName()).toBe('shelf');
    expect(localStorage.getItem('selected_database')).toBe('closet');
    expect(localStorage.getItem('selected_schema')).toBe('shelf');

    service.setSelectedContext(null, null);
    expect(localStorage.getItem('selected_database')).toBeNull();
    expect(localStorage.getItem('selected_schema')).toBeNull();
  });

  it('loads records and clears record loading failures', async () => {
    invokeMock.mockResolvedValueOnce([{ color: 'blue' }]);
    service.readRecords('closet', 'shelf', 'shirts');
    await vi.waitFor(() => expect(service.currentDrawerRecords()).toEqual([{ color: 'blue' }]));
    expect(service.isLoadingRecords()).toBe(false);

    vi.spyOn(console, 'error').mockImplementation(() => undefined);
    invokeMock.mockRejectedValueOnce(new Error('failed'));
    service.readRecords('closet', 'shelf', 'shirts');
    await vi.waitFor(() => expect(service.isLoadingRecords()).toBe(false));
    expect(service.currentDrawerRecords()).toEqual([]);
  });

  it('creates a record and refreshes its drawer', async () => {
    invokeMock
      .mockResolvedValueOnce(undefined)
      .mockResolvedValueOnce([{ color: 'blue' }]);

    await service.createRecord('closet', 'shelf', 'shirts', { color: 'blue' });
    await vi.waitFor(() => expect(service.currentDrawerRecords()).toEqual([{ color: 'blue' }]));

    expect(invokeMock).toHaveBeenNthCalledWith(1, 'wardrobe_create_record', {
      databaseName: 'closet',
      schemaName: 'shelf',
      drawerName: 'shirts',
      payload: { color: 'blue' },
    });
  });

  it('loads, removes, renames, and deletes saved connections', async () => {
    invokeMock
      .mockResolvedValueOnce([{ target: '/data/one' }])
      .mockResolvedValueOnce(undefined)
      .mockResolvedValueOnce([])
      .mockResolvedValueOnce(undefined)
      .mockResolvedValueOnce([{ target: '/data/two' }])
      .mockResolvedValueOnce(undefined)
      .mockResolvedValueOnce([]);

    await service.loadSavedConnections();
    expect(service.savedConnections()).toEqual([{ target: '/data/one' }]);

    await service.removeSavedConnection('one');
    expect(service.savedConnections()).toEqual([]);

    await service.updateConnectionAlias('/data/two', 'Two');
    expect(service.savedConnections()).toEqual([{ target: '/data/two' }]);

    service.databaseStatus.set('connected');
    service.activeConnectionPath.set('/data/two');
    await service.deleteConnectionFiles('/data/two', 'two');
    expect(service.databaseStatus()).toBe('disconnected');
    expect(service.activeConnectionPath()).toBeNull();
    expect(service.savedConnections()).toEqual([]);
  });

  it('creates a wardrobe and refreshes database inventory', async () => {
    invokeMock
      .mockResolvedValueOnce(undefined)
      .mockResolvedValueOnce([{ name: 'closet' }]);

    await service.createNewWardrobe('closet');
    await vi.waitFor(() => expect(service.databases()).toEqual([{ name: 'closet' }]));
  });

  it('absorbs saved-connection mutation failures', async () => {
    const consoleError = vi.spyOn(console, 'error').mockImplementation(() => undefined);
    invokeMock.mockRejectedValue(new Error('failed'));

    await service.loadSavedConnections();
    await service.removeSavedConnection('one');
    await service.updateConnectionAlias('one', 'One');
    await service.deleteConnectionFiles('one', 'one');
    await service.createNewWardrobe('closet');

    expect(consoleError).toHaveBeenCalledTimes(5);
  });
});
