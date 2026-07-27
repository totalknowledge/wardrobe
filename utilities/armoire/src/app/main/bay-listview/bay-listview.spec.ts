import { ComponentFixture, TestBed } from '@angular/core/testing';

import { BayListviewComponent } from './bay-listview';

describe('BayListview', () => {
  let component: BayListviewComponent;
  let fixture: ComponentFixture<BayListviewComponent>;

  beforeEach(async () => {
    (globalThis as any).__TAURI__ = {
      core: { invoke: vi.fn().mockResolvedValue([]) },
    };
    await TestBed.configureTestingModule({
      imports: [BayListviewComponent],
    }).compileComponents();

    fixture = TestBed.createComponent(BayListviewComponent);
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

  it('emits selected bays', () => {
    const selected = vi.fn();
    component.baySelected.subscribe(selected);

    component.selectBay('shelf');

    expect(selected).toHaveBeenCalledWith('shelf');
  });

  it('requires a database before creating a bay', () => {
    const create = vi.spyOn(component.databaseService, 'createNewBay');
    vi.stubGlobal('prompt', vi.fn(() => 'shelf'));

    component.createBay();

    expect(create).not.toHaveBeenCalled();
  });

  it('creates a trimmed bay for the selected database', () => {
    component.selectedDatabase = 'closet';
    const create = vi.spyOn(component.databaseService, 'createNewBay');
    vi.stubGlobal('prompt', vi.fn(() => '  shelf  '));

    component.createBay();

    expect(create).toHaveBeenCalledWith('closet', 'shelf');
  });

  it('ignores empty bay names', () => {
    component.selectedDatabase = 'closet';
    const create = vi.spyOn(component.databaseService, 'createNewBay');
    vi.stubGlobal('prompt', vi.fn(() => '   '));

    component.createBay();

    expect(create).not.toHaveBeenCalled();
  });

  it('renders unselected, loading, empty, and populated states', () => {
    fixture.detectChanges();
    expect(fixture.nativeElement.textContent).toContain('No Wardrobe Selected');

    fixture.componentRef.setInput('selectedDatabase', 'closet');
    component.databaseService.isLoadingBays.set(true);
    fixture.detectChanges();
    expect(fixture.nativeElement.querySelector('.loading-spinner')).toBeTruthy();

    component.databaseService.isLoadingBays.set(false);
    component.databaseService.bays.set([]);
    fixture.detectChanges();
    expect(fixture.nativeElement.textContent).toContain('No bays found');

    component.databaseService.bays.set(['shelf']);
    fixture.detectChanges();
    const bay = fixture.nativeElement.querySelector('li a') as HTMLAnchorElement;
    const selected = vi.fn();
    component.baySelected.subscribe(selected);
    bay.click();
    expect(selected).toHaveBeenCalledWith('shelf');
  });
});
