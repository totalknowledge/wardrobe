import { By } from '@angular/platform-browser';
import { ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter, Router } from '@angular/router';

import { BayListviewComponent } from './bay-listview/bay-listview';
import { ConnectionTreeComponent } from './connection-tree/connection-tree';
import { DrawerListviewComponent } from './drawer-listview/drawer-listview';
import { MainComponent } from './main';

describe('Main', () => {
  let component: MainComponent;
  let fixture: ComponentFixture<MainComponent>;
  let router: Router;

  beforeEach(async () => {
    (globalThis as any).__TAURI__ = {
      core: { invoke: vi.fn().mockResolvedValue([]) },
    };
    await TestBed.configureTestingModule({
      imports: [MainComponent],
      providers: [provideRouter([])],
    }).compileComponents();

    fixture = TestBed.createComponent(MainComponent);
    component = fixture.componentInstance;
    router = TestBed.inject(Router);
    await fixture.whenStable();
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it('should create', () => {
    expect(component).toBeTruthy();
  });

  it('reports active connection and bay layout state', () => {
    expect(component.hasActiveConnection).toBe(false);
    expect(component.hasBays).toBe(false);

    component.databaseService.databaseStatus.set('connected');
    component.databaseService.activeConnectionPath.set('/data/wardrobe');
    expect(component.hasActiveConnection).toBe(true);

    component.databaseService.bays.set(['shelf']);
    expect(component.hasBays).toBe(true);
  });

  it('clears selected context', () => {
    component.selectedDatabase = 'closet';
    component.selectedBay = 'shelf';
    const setContext = vi.spyOn(component.databaseService, 'setSelectedContext');

    component.clearSelection();

    expect(component.selectedDatabase).toBeNull();
    expect(component.selectedBay).toBeNull();
    expect(setContext).toHaveBeenCalledWith(null, null);
  });

  it('selects a database and loads its bays and default drawers', () => {
    const setContext = vi.spyOn(component.databaseService, 'setSelectedContext');
    const showBays = vi.spyOn(component.databaseService, 'showBays');
    const showDrawers = vi.spyOn(component.databaseService, 'showDrawers');

    component.selectDatabase('closet');

    expect(component.selectedDatabase).toBe('closet');
    expect(component.selectedBay).toBeNull();
    expect(setContext).toHaveBeenCalledWith('closet', null);
    expect(showBays).toHaveBeenCalledWith('closet');
    expect(showDrawers).toHaveBeenCalledWith('closet', 'default');
  });

  it('selects a bay only after a database is selected', () => {
    const setContext = vi.spyOn(component.databaseService, 'setSelectedContext');
    const showDrawers = vi.spyOn(component.databaseService, 'showDrawers');

    component.selectBay('shelf');
    expect(setContext).not.toHaveBeenCalled();

    component.selectedDatabase = 'closet';
    component.selectBay('shelf');

    expect(component.selectedBay).toBe('shelf');
    expect(setContext).toHaveBeenCalledWith('closet', 'shelf');
    expect(showDrawers).toHaveBeenCalledWith('closet', 'shelf');
  });

  it('navigates to the selected drawer', () => {
    const navigate = vi.spyOn(router, 'navigate').mockResolvedValue(true);

    component.viewDrawer('shirts');

    expect(navigate).toHaveBeenCalledWith(['/drawer', 'shirts']);
  });

  it('renders both two-column and three-column layouts', () => {
    component.databaseService.databaseStatus.set('connected');
    component.databaseService.activeConnectionPath.set('/data/wardrobe');
    component.databaseService.bays.set([]);
    fixture.detectChanges();
    const grid = fixture.nativeElement.querySelector('.grid') as HTMLElement;
    expect(grid.style.gridTemplateColumns).toContain('2fr');

    component.databaseService.bays.set(['shelf']);
    fixture.detectChanges();
    expect(grid.style.gridTemplateColumns).toContain('1.4fr');
    expect(fixture.nativeElement.querySelector('app-bay-listview')).toBeTruthy();
  });

  it('handles child component output events through the rendered layout', () => {
    component.databaseService.databaseStatus.set('connected');
    component.databaseService.activeConnectionPath.set('/data/wardrobe');
    component.databaseService.bays.set(['shelf']);
    const navigate = vi.spyOn(router, 'navigate').mockResolvedValue(true);
    fixture.detectChanges();

    const connectionTree = fixture.debugElement.query(
      By.directive(ConnectionTreeComponent),
    ).componentInstance as ConnectionTreeComponent;
    const bayList = fixture.debugElement.query(
      By.directive(BayListviewComponent),
    ).componentInstance as BayListviewComponent;
    const drawerList = fixture.debugElement.query(
      By.directive(DrawerListviewComponent),
    ).componentInstance as DrawerListviewComponent;

    connectionTree.databaseSelected.emit('closet');
    expect(component.selectedDatabase).toBe('closet');
    bayList.baySelected.emit('shelf');
    expect(component.selectedBay).toBe('shelf');
    drawerList.drawerSelected.emit('shirts');
    expect(navigate).toHaveBeenCalledWith(['/drawer', 'shirts']);
    connectionTree.connectionCleared.emit();
    expect(component.selectedDatabase).toBeNull();
    expect(component.selectedBay).toBeNull();
  });
});
