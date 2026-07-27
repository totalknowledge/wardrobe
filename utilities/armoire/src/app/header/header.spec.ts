import { ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';

import { DialogService } from '../dialog/dialog-service';
import { WardrobeService } from '../wardrobe/wardrobe-service';
import { HeaderComponent } from './header';

describe('HeaderComponent', () => {
  let component: HeaderComponent;
  let fixture: ComponentFixture<HeaderComponent>;
  let databaseService: WardrobeService;
  let dialogService: DialogService;

  beforeEach(async () => {
    (globalThis as any).__TAURI__ = {
      core: { invoke: vi.fn().mockResolvedValue([]) },
    };
    await TestBed.configureTestingModule({
      imports: [HeaderComponent],
      providers: [
        provideRouter([])
      ]
    }).compileComponents();

    fixture = TestBed.createComponent(HeaderComponent);
    component = fixture.componentInstance;
    databaseService = TestBed.inject(WardrobeService);
    dialogService = TestBed.inject(DialogService);
    await fixture.whenStable();
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it('should create', () => {
    expect(component).toBeTruthy();
  });

  it('closes menu details elements', () => {
    const menu = document.createElement('details');
    menu.setAttribute('open', '');

    component.closeMenus(menu);

    expect(menu.hasAttribute('open')).toBe(false);
  });

  it('opens connection modals and closes their menu', () => {
    const menu = document.createElement('details');
    menu.setAttribute('open', '');

    component.openConnectionModalAndClose(menu, 'connection');

    expect(dialogService.showConnectionModal()).toBe(true);
    expect(dialogService.connectionModalType).toBe('connection');
    expect(menu.hasAttribute('open')).toBe(false);
  });

  it('explains that a wardrobe must be selected before creating a bay', () => {
    component.openCreateBayDialog();

    expect(dialogService.dialogs()?.title).toBe('Create Bay');
    expect(dialogService.dialogs()?.body).toContain('Select a database first');
    expect(dialogService.dialogs()?.onConfirm).toBeUndefined();
  });

  it('creates a trimmed bay for the selected wardrobe', () => {
    databaseService.selectedDatabaseName.set('closet');
    const createBay = vi.spyOn(databaseService, 'createNewBay');

    component.openCreateBayDialog();
    dialogService.dialogs()?.onConfirm?.('  shelf  ');
    dialogService.dialogs()?.onConfirm?.('   ');

    expect(dialogService.dialogs()?.body).toContain('"closet"');
    expect(createBay).toHaveBeenCalledOnce();
    expect(createBay).toHaveBeenCalledWith('closet', 'shelf');
  });

  it('only disconnects connected active sources', () => {
    const disconnect = vi.spyOn(databaseService, 'disconnect');

    component.disconnect();
    expect(component.canDisconnect()).toBe(false);
    expect(disconnect).not.toHaveBeenCalled();

    databaseService.databaseStatus.set('connected');
    databaseService.activeConnectionPath.set('/data/wardrobe');
    expect(component.canDisconnect()).toBe(true);
    component.disconnect();
    expect(disconnect).toHaveBeenCalledOnce();
  });

  it('guards menu disconnection and closes the menu after success', () => {
    const menu = document.createElement('details');
    menu.setAttribute('open', '');
    component.disconnectAndClose(menu);
    expect(menu.hasAttribute('open')).toBe(true);

    databaseService.databaseStatus.set('connected');
    databaseService.activeConnectionPath.set('/data/wardrobe');
    component.disconnectAndClose(menu);
    expect(menu.hasAttribute('open')).toBe(false);
  });

  it('opens the about dialog and closes its menu', () => {
    const menu = document.createElement('details');
    menu.setAttribute('open', '');

    component.aboutAndClose(menu);

    expect(dialogService.dialogs()?.title).toBe('About Armoire');
    expect(dialogService.dialogs()?.body).toContain('Wardrobe database engine');
    expect(dialogService.dialogs()?.version).toBeTruthy();
    expect(menu.hasAttribute('open')).toBe(false);
  });

  it('handles header menu clicks through the rendered template', () => {
    databaseService.databaseStatus.set('connected');
    databaseService.activeConnectionPath.set('/data/wardrobe');
    databaseService.selectedDatabaseName.set('closet');
    const createBay = vi.spyOn(databaseService, 'createNewBay');
    fixture.detectChanges();

    const links = Array.from(
      fixture.nativeElement.querySelectorAll('a'),
    ) as HTMLAnchorElement[];
    const click = (label: string) => {
      const link = links.find((item) => item.textContent?.replace(/\s/g, '').includes(label));
      expect(link).toBeTruthy();
      link?.click();
    };

    click('NewConnection');
    expect(dialogService.connectionModalType).toBe('connection');
    click('NewFileLocation');
    expect(dialogService.connectionModalType).toBe('location');
    click('NewBay');
    dialogService.dialogs()?.onConfirm?.('shelf');
    expect(createBay).toHaveBeenCalledWith('closet', 'shelf');
    click('About');
    expect(dialogService.dialogs()?.title).toBe('About Armoire');
    click('Disconnect');
    expect(databaseService.databaseStatus()).toBe('disconnected');
  });
});
