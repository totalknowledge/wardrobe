import { ComponentFixture, TestBed } from '@angular/core/testing';
import { convertToParamMap, provideRouter, Router } from '@angular/router';
import { of } from 'rxjs';

import { Drawer } from './drawer';

describe('Drawer', () => {
  let component: Drawer;
  let fixture: ComponentFixture<Drawer>;
  let router: Router;

  beforeEach(async () => {
    (globalThis as any).__TAURI__ = {
      core: { invoke: vi.fn().mockResolvedValue([]) },
    };
    await TestBed.configureTestingModule({
      imports: [Drawer],
      providers: [provideRouter([])],
    }).compileComponents();

    fixture = TestBed.createComponent(Drawer);
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

  it('loads records from the current selected context', () => {
    component.databaseService.selectedDatabaseName.set('closet');
    component.databaseService.selectedSchemaName.set('shelf');
    const read = vi.spyOn(component.databaseService, 'readRecords');
    (component as any).route = {
      paramMap: of(convertToParamMap({ drawerName: 'shirts' })),
    };

    component.ngOnInit();

    expect(component.databaseName).toBe('closet');
    expect(component.schemaName).toBe('shelf');
    expect(component.drawerName).toBe('shirts');
    expect(read).toHaveBeenCalledWith('closet', 'shelf', 'shirts');
  });

  it('navigates back to the wardrobe and bay contexts', () => {
    component.databaseName = 'closet';
    component.schemaName = 'shelf';
    const setContext = vi.spyOn(component.databaseService, 'setSelectedContext');
    const navigate = vi.spyOn(router, 'navigate').mockResolvedValue(true);

    component.goToWardrobe();
    component.goToBay();

    expect(setContext).toHaveBeenNthCalledWith(1, 'closet', null);
    expect(setContext).toHaveBeenNthCalledWith(2, 'closet', 'shelf');
    expect(navigate).toHaveBeenCalledTimes(2);
    expect(navigate).toHaveBeenCalledWith(['/']);
  });

  it('opens and closes the create-record modal', () => {
    component.openCreateModal();

    expect(component.showCreateModal).toBe(true);
    expect(JSON.parse(component.newRecordJson)).toEqual({ name: '' });
    expect(component.jsonError).toBeNull();

    component.jsonError = 'old error';
    component.closeCreateModal();
    expect(component.showCreateModal).toBe(false);
    expect(component.newRecordJson).toBe('');
    expect(component.jsonError).toBeNull();
  });

  it('rejects invalid record JSON', () => {
    component.newRecordJson = '{invalid';

    component.saveRecord();

    expect(component.isSaving).toBe(false);
    expect(component.jsonError).toContain('Invalid JSON format');
  });

  it('saves valid records and closes the modal', async () => {
    component.databaseName = 'closet';
    component.schemaName = 'shelf';
    component.drawerName = 'shirts';
    component.showCreateModal = true;
    component.newRecordJson = '{"_id":"blue-shirt","color":"blue"}';
    const create = vi.spyOn(component.databaseService, 'createRecord')
      .mockResolvedValue(undefined);

    component.saveRecord();
    expect(component.isSaving).toBe(true);

    await vi.waitFor(() => expect(component.isSaving).toBe(false));
    expect(create).toHaveBeenCalledWith(
      'closet',
      'shelf',
      'shirts',
      { _id: 'blue-shirt', color: 'blue' },
    );
    expect(component.showCreateModal).toBe(false);
  });

  it('reports record creation failures', async () => {
    component.databaseName = 'closet';
    component.schemaName = 'shelf';
    component.drawerName = 'shirts';
    component.newRecordJson = '{"_id":"blue-shirt"}';
    vi.spyOn(component.databaseService, 'createRecord')
      .mockRejectedValue(new Error('write failed'));

    component.saveRecord();

    await vi.waitFor(() => expect(component.isSaving).toBe(false));
    expect(component.jsonError).toBe('write failed');
  });

  it('formats records as indented JSON', () => {
    expect(component.getJsonString({ color: 'blue' })).toBe(
      '{\n  "color": "blue"\n}',
    );
  });

  it('renders loading, empty, populated, and create modal states', () => {
    component.databaseName = 'closet';
    component.schemaName = 'shelf';
    component.drawerName = 'shirts';
    component.databaseService.isLoadingRecords.set(true);
    fixture.changeDetectorRef.markForCheck();
    fixture.detectChanges();
    expect(fixture.nativeElement.querySelector('.loading-spinner')).toBeTruthy();

    component.databaseService.isLoadingRecords.set(false);
    component.databaseService.currentDrawerRecords.set([]);
    fixture.detectChanges();
    expect(fixture.nativeElement.textContent).toContain('No records found');

    component.databaseService.currentDrawerRecords.set([
      { _id: 'blue-shirt', name: 'Blue Shirt', color: 'blue' },
    ]);
    fixture.detectChanges();
    expect(fixture.nativeElement.textContent).toContain('blue-shirt');
    expect(fixture.nativeElement.textContent).toContain('Blue Shirt');

    const createButton = Array.from(
      fixture.nativeElement.querySelectorAll('button'),
    ).find((button: any) => button.textContent?.includes('Create Record')) as HTMLButtonElement;
    createButton.click();
    fixture.detectChanges();
    expect(fixture.nativeElement.textContent).toContain('New Record - shirts');

    component.newRecordJson = '{invalid';
    const saveButton = Array.from(
      fixture.nativeElement.querySelectorAll('button'),
    ).find((button: any) => button.textContent?.includes('Save Record')) as HTMLButtonElement;
    saveButton.click();
    fixture.detectChanges();
    expect(fixture.nativeElement.textContent).toContain('Invalid JSON format');

    component.newRecordJson = '{"_id":"pending"}';
    vi.spyOn(component.databaseService, 'createRecord')
      .mockReturnValue(new Promise(() => undefined));
    saveButton.click();
    fixture.detectChanges();
    expect(fixture.nativeElement.textContent).toContain('Saving...');
  });
});
