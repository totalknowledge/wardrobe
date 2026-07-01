import { Routes } from '@angular/router';
import { MainComponent } from './main/main';
import { Drawer } from './drawer/drawer';
import { Settings } from './settings/settings';

export const routes: Routes = [
    {
        path: '',
        component: MainComponent,
    },
    {
        path: 'drawer/:drawerName',
        component: Drawer,
    },
    {
        path: 'settings',
        component: Settings,
    },
];
