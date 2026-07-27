import { ComponentFixture, TestBed } from '@angular/core/testing';

import { DialogService } from '../../dialog/dialog-service';
import { ConnectionTreeComponent } from './connection-tree';

describe('ConnectionTree', () => {
  let component: ConnectionTreeComponent;
  let fixture: ComponentFixture<ConnectionTreeComponent>;
  let dialogService: DialogService;

  beforeEach(async () => {
    (globalThis as any).__TAURI__ = {
      core: { invoke: vi.fn().mockResolvedValue([]) },
    };
    await TestBed.configureTestingModule({
      imports: [ConnectionTreeComponent],
    }).compileComponents();

    fixture = TestBed.createComponent(ConnectionTreeComponent);
    component = fixture.componentInstance;
    dialogService = TestBed.inject(DialogService);
    await fixture.whenStable();
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });

  it('should create', () => {
    expect(component).toBeTruthy();
  });

  it('creates a trimmed wardrobe name from the prompt', () => {
    const create = vi.spyOn(component.databaseService, 'createNewWardrobe')
      .mockResolvedValue(undefined);
    vi.stubGlobal('prompt', vi.fn(() => '  closet  '));

    component.createWardrobe();

    expect(create).toHaveBeenCalledWith('closet');
  });

  it('ignores empty wardrobe prompts', () => {
    const create = vi.spyOn(component.databaseService, 'createNewWardrobe')
      .mockResolvedValue(undefined);
    vi.stubGlobal('prompt', vi.fn(() => '   '));

    component.createWardrobe();

    expect(create).not.toHaveBeenCalled();
  });

  it('disconnects an already active saved connection', () => {
    component.databaseService.activeConnectionPath.set('/data/wardrobe');
    const disconnect = vi.spyOn(component.databaseService, 'disconnect');
    const cleared = vi.fn();
    component.connectionCleared.subscribe(cleared);

    component.selectSavedConnection('/data/wardrobe');

    expect(disconnect).toHaveBeenCalledOnce();
    expect(cleared).toHaveBeenCalledOnce();
  });

  it('connects a saved target and selects its only wardrobe', async () => {
    const connect = vi.spyOn(component.databaseService, 'connectSourceLocation')
      .mockResolvedValue([{ name: 'closet' }]);
    const selected = vi.fn();
    const cleared = vi.fn();
    component.databaseSelected.subscribe(selected);
    component.connectionCleared.subscribe(cleared);

    component.selectSavedConnection('/data/wardrobe');

    await vi.waitFor(() => expect(selected).toHaveBeenCalledWith('closet'));
    expect(connect).toHaveBeenCalledWith('/data/wardrobe');
    expect(cleared).toHaveBeenCalledOnce();
  });

  it('does not auto-select when a connection has multiple wardrobes', async () => {
    vi.spyOn(component.databaseService, 'connectSourceLocation')
      .mockResolvedValue([{ name: 'one' }, { name: 'two' }]);
    const selected = vi.fn();
    component.databaseSelected.subscribe(selected);

    component.selectSavedConnection('/data/wardrobe');

    await Promise.resolve();
    expect(selected).not.toHaveBeenCalled();
  });

  it('emits direct database selections and identifies local targets', () => {
    const selected = vi.fn();
    component.databaseSelected.subscribe(selected);

    component.selectDatabase('closet');

    expect(selected).toHaveBeenCalledWith('closet');
    expect(component.isFlatfile('/data/wardrobe')).toBe(true);
    expect(component.isFlatfile('wardrobe://server:24842')).toBe(false);
    expect(component.isFlatfile('https://server')).toBe(false);
  });

  it('opens local connection actions and renames a connection', () => {
    vi.useFakeTimers();
    const stopPropagation = vi.fn();
    const rename = vi.spyOn(component.databaseService, 'updateConnectionAlias')
      .mockResolvedValue(undefined);

    component.openConnectionMenu(
      { stopPropagation } as unknown as Event,
      { _id: 'local', target: '/data/wardrobe', name: 'Local' },
    );

    expect(stopPropagation).toHaveBeenCalledOnce();
    expect(dialogService.dialogs()?.actions).toHaveLength(3);
    dialogService.dialogs()?.actions?.[0].onClick();
    vi.advanceTimersByTime(150);
    expect(dialogService.dialogs()?.title).toBe('Rename Connection');

    dialogService.dialogs()?.onConfirm?.('Renamed');
    expect(rename).toHaveBeenCalledWith('/data/wardrobe', 'Renamed');
  });

  it('removes an active connection and clears its selection', () => {
    component.databaseService.activeConnectionPath.set('/data/wardrobe');
    const remove = vi.spyOn(component.databaseService, 'removeSavedConnection')
      .mockResolvedValue(undefined);
    const disconnect = vi.spyOn(component.databaseService, 'disconnect');
    const cleared = vi.fn();
    component.connectionCleared.subscribe(cleared);

    component.openConnectionMenu(
      { stopPropagation: vi.fn() } as unknown as Event,
      { _id: 'local', target: '/data/wardrobe' },
    );
    dialogService.dialogs()?.actions?.[1].onClick();

    expect(remove).toHaveBeenCalledWith('local');
    expect(disconnect).toHaveBeenCalledOnce();
    expect(cleared).toHaveBeenCalledOnce();
  });

  it('offers no file deletion action for remote connections', () => {
    component.openConnectionMenu(
      { stopPropagation: vi.fn() } as unknown as Event,
      { _id: 'remote', target: 'wardrobe://server:24842' },
    );

    expect(dialogService.dialogs()?.actions).toHaveLength(2);
  });

  it('deletes confirmed local connection files and honors cancellation', () => {
    const deleteFiles = vi.spyOn(component.databaseService, 'deleteConnectionFiles')
      .mockResolvedValue(undefined);
    const confirmMock = vi.fn()
      .mockReturnValueOnce(true)
      .mockReturnValueOnce(false);
    vi.stubGlobal('confirm', confirmMock);

    (component as any).deleteConnection('/data/wardrobe', 'local');
    (component as any).deleteConnection('/data/other', 'other');

    expect(deleteFiles).toHaveBeenCalledOnce();
    expect(deleteFiles).toHaveBeenCalledWith('/data/wardrobe', 'local');
  });

  it('renders saved, active, empty, and loading connection states', () => {
    component.databaseService.databaseStatus.set('connecting');
    component.databaseService.activeConnectionPath.set('/data/wardrobe');
    component.databaseService.savedConnections.set([
      { _id: 'local', target: '/data/wardrobe', name: 'Local' },
    ]);
    component.databaseService.databases.set([{ name: 'closet', record_count: 2 }]);
    component.selectedDatabase = 'closet';
    fixture.detectChanges();

    expect(fixture.nativeElement.textContent).toContain('Local');
    expect(fixture.nativeElement.textContent).toContain('closet');
    expect(fixture.nativeElement.textContent).toContain('2 records');

    component.databaseService.databaseStatus.set('connected');
    component.databaseService.databases.set([]);
    fixture.detectChanges();
    expect(fixture.nativeElement.textContent).toContain('No wardrobes found');

    component.databaseService.savedConnections.set([]);
    fixture.detectChanges();
    expect(fixture.nativeElement.textContent).toContain('No connections saved');
  });
});
