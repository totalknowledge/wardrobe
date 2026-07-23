import { ComponentFixture, TestBed } from '@angular/core/testing';

import { DrawerListviewComponent } from './drawer-listview';

describe('DrawerListview', () => {
  let component: DrawerListviewComponent;
  let fixture: ComponentFixture<DrawerListviewComponent>;

  beforeEach(async () => {
    await TestBed.configureTestingModule({
      imports: [DrawerListviewComponent],
    }).compileComponents();

    fixture = TestBed.createComponent(DrawerListviewComponent);
    component = fixture.componentInstance;
    await fixture.whenStable();
  });

  it('should create', () => {
    expect(component).toBeTruthy();
  });
});
