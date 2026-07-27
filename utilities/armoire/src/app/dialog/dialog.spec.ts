import { ComponentFixture, TestBed } from '@angular/core/testing';

import { DialogComponent } from './dialog';

describe('DialogComponent', () => {
  let component: DialogComponent;
  let fixture: ComponentFixture<DialogComponent>;
  let invokeMock: ReturnType<typeof vi.fn>;

  beforeEach(async () => {
    invokeMock = vi.fn().mockResolvedValue([]);
    (globalThis as any).__TAURI__ = {
      core: { invoke: invokeMock },
    };
    await TestBed.configureTestingModule({
      imports: [DialogComponent],
    }).compileComponents();

    fixture = TestBed.createComponent(DialogComponent);
    component = fixture.componentInstance;
    await fixture.whenStable();
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it('should create', () => {
    expect(component).toBeTruthy();
  });

  it('exposes dialog content, actions, confirmation, and defaults', () => {
    const action = { label: 'Run', onClick: vi.fn() };
    component.dialogService.openDialog({
      title: 'Title',
      body: 'Body',
      version: '1.2.3',
      showInput: true,
      inputPlaceholder: 'Name',
      confirmText: 'Save',
      cancelText: 'Dismiss',
      onConfirm: vi.fn(),
      actions: [action],
    });

    expect(component.dialogTitle).toBe('Title');
    expect(component.dialogBody).toBe('Body');
    expect(component.dialogVersion).toBe('1.2.3');
    expect(component.dialogShowInput).toBe(true);
    expect(component.dialogInputPlaceholder).toBe('Name');
    expect(component.dialogActions).toEqual([action]);
    expect(component.dialogHasActions).toBe(true);
    expect(component.dialogHasConfirm).toBe(true);
    expect(component.dialogConfirmText).toBe('Save');
    expect(component.dialogCancelText).toBe('Dismiss');

    component.dialogService.closeDialog();
    expect(component.dialogTitle).toBe('');
    expect(component.dialogBody).toBe('');
    expect(component.dialogVersion).toBe('');
    expect(component.dialogShowInput).toBe(false);
    expect(component.dialogInputPlaceholder).toBe('');
    expect(component.dialogActions).toEqual([]);
    expect(component.dialogHasActions).toBe(false);
    expect(component.dialogHasConfirm).toBe(false);
    expect(component.dialogConfirmText).toBe('Confirm');
    expect(component.dialogCancelText).toBe('Cancel');
  });

  it('delegates connection modal fields to the dialog service', () => {
    component.dialogService.openConnectionModal('location');
    component.fileLocationPath = '/data/wardrobe';
    component.createIfNotExist = true;
    component.connectionUri = 'server';
    component.connectionName = 'Local';

    expect(component.showConnectionModal).toBe(true);
    expect(component.connectionModalType).toBe('location');
    expect(component.fileLocationPath).toBe('/data/wardrobe');
    expect(component.createIfNotExist).toBe(true);
    expect(component.connectionUri).toBe('server');
    expect(component.connectionName).toBe('Local');
    expect(component.connectionError).toBeNull();
    expect(component.connectionTestMessage).toBeNull();
    expect(component.isConnecting).toBe(false);
    expect(component.isTestingConnection).toBe(false);

    component.closeConnectionModal();
    expect(component.showConnectionModal).toBe(false);
  });

  it('uses a selected file path and guesses a location name', () => {
    component.dialogService.openConnectionModal('location');
    const file = Object.assign(new File([''], 'catalog.drw'), {
      webkitRelativePath: 'closet/catalog.drw',
    });

    component.onFolderSelected({
      target: { files: [file] },
    } as unknown as Event);

    expect(component.fileLocationPath).toBe('closet/catalog.drw');
    expect(component.connectionName).toBe('catalog.drw');
  });

  it('ignores empty folder selections and preserves manually edited names', () => {
    component.dialogService.openConnectionModal('location');
    component.connectionName = 'Manual';
    component.onConnectionNameChanged();

    component.onFolderSelected({ target: { files: [] } } as unknown as Event);
    component.onConnectionTargetChanged('/data/automatic');

    expect(component.connectionName).toBe('Manual');
    expect(component.dialogService.connectionNameEdited).toBe(true);
  });

  it('guesses normalized server and filesystem connection names', () => {
    component.dialogService.openConnectionModal('connection');
    component.onConnectionTargetChanged('db.example.com');
    expect(component.connectionName).toBe('db.example.com:24842');

    component.onConnectionTargetChanged('db.example.com:9000');
    expect(component.connectionName).toBe('db.example.com:9000');

    component.dialogService.openConnectionModal('location');
    component.onConnectionTargetChanged('/data/closet/');
    expect(component.connectionName).toBe('closet');

    component.onConnectionTargetChanged('');
    expect(component.connectionName).toBe('');
  });

  it('does not test a server while the location modal is active', () => {
    component.dialogService.openConnectionModal('location');
    component.testServerConnection();
    expect(invokeMock).not.toHaveBeenCalled();
  });

  it('normalizes and tests a server connection successfully', async () => {
    component.dialogService.openConnectionModal('connection');
    component.connectionUri = 'db.example.com';
    invokeMock.mockResolvedValue(undefined);

    component.testServerConnection();
    expect(component.isTestingConnection).toBe(true);

    await vi.waitFor(() => expect(component.isTestingConnection).toBe(false));
    expect(component.connectionTestMessage).toBe('Connection test succeeded.');
    expect(invokeMock).toHaveBeenCalledWith('wardrobe_test_database_access', {
      databaseDirectory: 'wardrobe://db.example.com:24842',
    });
  });

  it('reports server connection test errors', async () => {
    component.dialogService.openConnectionModal('connection');
    component.connectionUri = 'wardrobe://offline';
    invokeMock.mockRejectedValue(new Error('offline'));

    component.testServerConnection();

    await vi.waitFor(() => expect(component.isTestingConnection).toBe(false));
    expect(component.connectionError).toBe('offline');
    expect(component.connectionTestMessage).toBeNull();
  });

  it('validates empty connection submissions and prevents enter defaults', () => {
    component.dialogService.openConnectionModal('location');
    const preventDefault = vi.fn();

    component.submitConnectionOnEnter({ preventDefault } as unknown as Event);

    expect(preventDefault).toHaveBeenCalledOnce();
    expect(component.connectionError).toBe('Please provide a valid target.');
    expect(component.isConnecting).toBe(false);
  });

  it('creates and connects a new local source location', async () => {
    component.dialogService.openConnectionModal('location');
    component.fileLocationPath = ' /data/wardrobe ';
    component.connectionName = ' Local ';
    component.createIfNotExist = true;
    invokeMock.mockImplementation((command: string) => Promise.resolve(
      command === 'wardrobe_show_wardrobes' || command === 'armoire_get_saved_connections'
        ? []
        : undefined,
    ));

    component.submitConnection();
    expect(component.isConnecting).toBe(true);

    await vi.waitFor(() => expect(component.showConnectionModal).toBe(false));
    expect(invokeMock).toHaveBeenCalledWith('wardrobe_create_source_location', {
      databaseDirectory: '/data/wardrobe',
    });
    expect(invokeMock).toHaveBeenCalledWith('wardrobe_connect_source_location', {
      databaseDirectory: '/data/wardrobe',
      name: 'Local',
    });
    expect(invokeMock).toHaveBeenCalledWith('armoire_get_saved_connections', undefined);
  });

  it('normalizes and connects an existing server target', async () => {
    component.dialogService.openConnectionModal('connection');
    component.connectionUri = 'db.example.com:9000';
    invokeMock.mockImplementation((command: string) => Promise.resolve(
      command === 'wardrobe_show_wardrobes' || command === 'armoire_get_saved_connections'
        ? []
        : undefined,
    ));

    component.submitConnection();

    await vi.waitFor(() => expect(component.showConnectionModal).toBe(false));
    expect(invokeMock).toHaveBeenCalledWith('wardrobe_connect_source_location', {
      databaseDirectory: 'wardrobe://db.example.com:9000',
      name: undefined,
    });
  });

  it('reports submission failures and keeps the modal open', async () => {
    component.dialogService.openConnectionModal('location');
    component.fileLocationPath = '/missing';
    invokeMock.mockRejectedValue('not found');
    vi.spyOn(console, 'error').mockImplementation(() => undefined);

    component.submitConnection();

    await vi.waitFor(() => expect(component.isConnecting).toBe(false));
    expect(component.connectionError).toBe('not found');
    expect(component.showConnectionModal).toBe(true);
  });

  it('renders dialog and connection modal variants', () => {
    component.dialogService.openDialog({
      title: 'Actions',
      body: 'Choose',
      version: '1.0.0',
      showInput: true,
      inputPlaceholder: 'Value',
      actions: [{ label: 'Run', onClick: vi.fn() }],
    });
    fixture.detectChanges();
    expect(fixture.nativeElement.textContent).toContain('Actions');
    expect(fixture.nativeElement.textContent).toContain('Version 1.0.0');

    component.dialogService.closeDialog();
    component.dialogService.openConnectionModal('connection');
    component.dialogService.connectionError = 'offline';
    component.dialogService.connectionTestMessage = 'ready';
    fixture.detectChanges();
    expect(fixture.nativeElement.textContent).toContain('New Connection');
    expect(fixture.nativeElement.textContent).toContain('offline');
    expect(fixture.nativeElement.textContent).toContain('ready');
  });
});
