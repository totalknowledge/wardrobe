import { ComponentFixture, TestBed } from '@angular/core/testing';

import { BayListviewComponent } from './bay-listview';

describe('BayListview', () => {
  let component: BayListviewComponent;
  let fixture: ComponentFixture<BayListviewComponent>;

  beforeEach(async () => {
    await TestBed.configureTestingModule({
      imports: [BayListviewComponent],
    }).compileComponents();

    fixture = TestBed.createComponent(BayListviewComponent);
    component = fixture.componentInstance;
    await fixture.whenStable();
  });

  it('should create', () => {
    expect(component).toBeTruthy();
  });
});
