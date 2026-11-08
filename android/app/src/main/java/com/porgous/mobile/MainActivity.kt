package com.porgous.mobile

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.viewModels
import com.porgous.mobile.ui.PorgousApp
import com.porgous.mobile.ui.PorgousTheme

class MainActivity : ComponentActivity() {
    private val viewModel: PanelViewModel by viewModels()

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContent {
            PorgousTheme {
                PorgousApp(viewModel)
            }
        }
    }
}
