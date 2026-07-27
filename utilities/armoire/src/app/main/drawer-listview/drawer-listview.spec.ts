import { ComponentFixture, TestBed } from '@angular/core/testing';

import { DrawerListviewComponent } from './drawer-listview';

describe('DrawerListview', () => {
  let component: DrawerListviewComponent;
  let fixture: ComponentFixture<DrawerListviewComponent>;

  beforeEach(async () => {
    (globalThis as any).__TAURI__ = {
      core: { invoke: vi.fn().mockResolvedValue([]) },
    };
    await TestBed.configureTestingModule({
      imports: [DrawerListviewComponent],
    }).compileComponents();

    fixture = TestBed.createComponent(DrawerListviewComponent);
    component = fixture.componentInstance;
    await fixture.whenStable();
  });

  afterEach(() => {
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });

  it('should create', () => {
    expect(component).toBeTruthy();
  });

  it('emits selected drawers', () => {
    const selected = vi.fn();
    component.drawerSelected.subscribe(selected);

    component.viewDrawer('shirts');

    expect(selected).toHaveBeenCalledWith('shirts');
  });

  it('requires a database before creating a drawer', () => {
    const create = vi.spyOn(component.databaseService, 'createNewDrawer');
    vi.stubGlobal('prompt', vi.fn(() => 'shirts'));

    component.createDrawer();

    expect(create).not.toHaveBeenCalled();
  });

  it('creates a trimmed drawer in the selected or default bay', () => {
    component.selectedDatabase = 'closet';
    const create = vi.spyOn(component.databaseService, 'createNewDrawer');
    vi.stubGlobal('prompt', vi.fn(() => '  shirts  '));

    component.createDrawer();
    component.selectedBay = 'shelf';
    component.createDrawer();

    expect(create).toHaveBeenNthCalledWith(1, 'closet', 'default', 'shirts');
    expect(create).toHaveBeenNthCalledWith(2, 'closet', 'shelf', 'shirts');
  });

  it('ignores empty drawer names', () => {
    component.selectedDatabase = 'closet';
    const create = vi.spyOn(component.databaseService, 'createNewDrawer');
    vi.stubGlobal('prompt', vi.fn(() => '   '));

    component.createDrawer();

    expect(create).not.toHaveBeenCalled();
  });

  it('renders unselected, loading, empty, and populated states', () => {
    fixture.detectChanges();
    expect(fixture.nativeElement.textContent).toContain('No Wardrobe Selected');

    fixture.componentRef.setInput('selectedDatabase', 'closet');
    component.databaseService.isLoadingDrawers.set(true);
    fixture.detectChanges();
    expect(fixture.nativeElement.querySelector('.loading-spinner')).toBeTruthy();

    component.databaseService.isLoadingDrawers.set(false);
    component.databaseService.drawers.set([]);
    fixture.detectChanges();
    expect(fixture.nativeElement.textContent).toContain('No drawers found');

    component.databaseService.drawers.set([{ name: 'shirts', record_count: 2 }]);
    fixture.detectChanges();
    const drawer = fixture.nativeElement.querySelector('li a') as HTMLAnchorElement;
    const selected = vi.fn();
    component.drawerSelected.subscribe(selected);
    drawer.click();
    expect(selected).toHaveBeenCalledWith('shirts');
    expect(fixture.nativeElement.textContent).toContain('2 records');
  });
});
